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
    Container, ContainerPort, EnvVar, PodSpec, PodTemplateSpec, Service, ServicePort, ServiceSpec,
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

/// Render the Application's `base` block (no environment override
/// applied). v0.1.30 entry point — keeps the simple call-site
/// shape; new code should prefer [`render_application_for_env`]
/// when the operator knows which environment it represents.
pub fn render_application(app: &Application) -> RenderedApplication {
    render_application_for_env(app, None)
}

/// Render the Application using the merged base + environment
/// override (when `env_name` is `Some(...)` and the override exists).
pub fn render_application_for_env(
    app: &Application,
    env_name: Option<&str>,
) -> RenderedApplication {
    let name = app.metadata.name.clone().unwrap_or_default();
    let namespace = app.metadata.namespace.clone();
    let owner = owner_reference(app);
    let labels = make_labels(&name);
    let effective = effective_spec(app, env_name);

    let deployment = render_deployment(&name, namespace.as_deref(), &owner, &labels, &effective);
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
) -> Deployment {
    let replicas = spec.replicas.unwrap_or(1);
    let image = spec.image.clone().unwrap_or_default();
    let container_port = spec.expose.as_ref().map(|e| ContainerPort {
        container_port: e.port,
        protocol: Some("TCP".to_string()),
        ..Default::default()
    });

    let env_vars: Vec<EnvVar> = spec
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
        use operator_core::application::ServiceNeed;
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
        let r = render_application_for_env(&app, Some("prod"));
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
}
