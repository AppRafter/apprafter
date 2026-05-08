// SPDX-License-Identifier: FSL-1.1-MIT
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

/// Render the Application's `base` block into a typed Deployment +
/// optional Service. Per-environment overrides are not applied
/// here — phase 1.9c folds them in via CUE unification.
pub fn render_application(app: &Application) -> RenderedApplication {
    let name = app.metadata.name.clone().unwrap_or_default();
    let namespace = app.metadata.namespace.clone();
    let owner = owner_reference(app);
    let labels = make_labels(&name);
    let base = app.spec.base.clone().unwrap_or_default();

    let deployment = render_deployment(&name, namespace.as_deref(), &owner, &labels, &base);
    let service = base
        .expose
        .as_ref()
        .map(|expose| render_service(&name, namespace.as_deref(), &owner, &labels, expose.port));

    RenderedApplication {
        deployment,
        service,
    }
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
        let envs = r.deployment.spec.as_ref().unwrap().template.spec.as_ref().unwrap().containers[0]
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
        let ports = r.deployment.spec.as_ref().unwrap().template.spec.as_ref().unwrap().containers[0]
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
}
