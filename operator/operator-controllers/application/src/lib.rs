// SPDX-License-Identifier: FSL-1.1-MIT
//! kube-rs Controller for the v1alpha1 `Application` CRD.
//!
//! v0.1.31 (sub-phase 1.9b) wires the v0.1.30 renderer into
//! `reconcile` via server-side apply with field manager
//! `apprafter-operator` and updates the Application's `status`
//! subresource (phase, observedGeneration, conditions, endpointURL).

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::StreamExt;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Service;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher;
use kube::{Client, ResourceExt};
use serde_json::{json, Value};
use thiserror::Error;
use tracing::{info, warn};

use operator_core::{Application, ApplicationCondition, ApplicationStatus, Metrics};
use operator_rendering::render_application_for_env;

/// Resource kind label used for every metric tagged with `kind`.
const KIND: &str = "Application";

/// SSA field manager. Each apply / status-patch tags the fields it
/// owns under this name, so future controllers (e.g. operators
/// extending Applications) can co-own without conflicts.
pub const FIELD_MANAGER: &str = "apprafter-operator";

/// Per-controller reconcile context.
pub struct Context {
    pub client: Client,
    pub metrics: Arc<Metrics>,
    /// Active environment name — when `Some(...)`, reconcile
    /// applies the matching `spec.environments[env_name]` override
    /// on top of `spec.base`. Sourced from `APPRAFTER_ENV` env var
    /// in the binary; `None` falls back to `spec.base` only.
    pub env_name: Option<String>,
}

#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error("kube-rs error: {0}")]
    Kube(#[from] kube::Error),

    #[error("serde_json error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Spawn the Application Controller. Watches `apprafter.io/v1alpha1`
/// `Application` resources cluster-wide and reconciles them through
/// [`reconcile`]. Errors from individual reconcile calls go through
/// [`error_policy`].
pub async fn run(
    client: Client,
    metrics: Arc<Metrics>,
    env_name: Option<String>,
) -> Result<(), ReconcileError> {
    let apps: Api<Application> = Api::all(client.clone());
    let context = Arc::new(Context {
        client,
        metrics,
        env_name,
    });

    Controller::new(apps, watcher::Config::default())
        .run(reconcile, error_policy, context)
        .for_each(|res| async move {
            match res {
                Ok((obj_ref, _action)) => {
                    info!(?obj_ref, "controller step ok");
                }
                Err(err) => {
                    warn!(%err, "controller step error");
                }
            }
        })
        .await;
    Ok(())
}

/// Reconcile fn — renders the Application, applies the children
/// via SSA, updates the Application status. Returns Action::requeue
/// 60s on success.
pub async fn reconcile(app: Arc<Application>, ctx: Arc<Context>) -> Result<Action, ReconcileError> {
    let name = app.name_any();
    let namespace = app.namespace().unwrap_or_default();
    let _timer = ctx
        .metrics
        .reconcile_duration
        .with_label_values(&[KIND])
        .start_timer();

    info!(%name, %namespace, env = ?ctx.env_name, "reconciling Application");

    let rendered = render_application_for_env(&app, ctx.env_name.as_deref());
    let pp = PatchParams::apply(FIELD_MANAGER).force();

    apply_deployment(&ctx.client, &namespace, &rendered.deployment, &pp).await?;

    if let Some(service) = &rendered.service {
        apply_service(&ctx.client, &namespace, service, &pp).await?;
    }

    let endpoint_url = rendered
        .service
        .as_ref()
        .and_then(|s| s.metadata.name.as_deref())
        .map(|svc_name| cluster_internal_endpoint_url(svc_name, &namespace, 80));

    // Preserve lastTransitionTime if the previous Ready condition
    // already had the same `status` value — per k8s convention the
    // timestamp moves only when `status` transitions. Without this,
    // each reconcile bumps the timestamp → status diff → watch
    // event on our own write → fresh reconcile → hot loop spinning
    // the operator's CPU.
    let previous_conditions = app
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_deref())
        .unwrap_or(&[]);
    let conditions = vec![ready_condition(
        "True",
        "ReconcileSucceeded",
        "Reconcile completed; child Deployment and Service applied.",
        previous_conditions,
    )];
    let status = build_status(&app, "Ready", conditions, endpoint_url);
    apply_status(&ctx.client, &namespace, &name, &status, &pp).await?;

    ctx.metrics
        .reconcile_total
        .with_label_values(&[KIND, &namespace, "ok"])
        .inc();
    Ok(Action::requeue(Duration::from_secs(60)))
}

/// Error policy — logs the error, increments the error counters,
/// and requeues with a fixed 30s delay. Phase 1.9c will distinguish
/// transient vs terminal errors and wire up exponential backoff.
pub fn error_policy(app: Arc<Application>, err: &ReconcileError, ctx: Arc<Context>) -> Action {
    let name = app.name_any();
    let namespace = app.namespace().unwrap_or_default();
    warn!(%name, %namespace, %err, "reconcile error");
    ctx.metrics
        .reconcile_total
        .with_label_values(&[KIND, &namespace, "error"])
        .inc();
    ctx.metrics
        .reconcile_errors
        .with_label_values(&[KIND])
        .inc();
    Action::requeue(Duration::from_secs(30))
}

async fn apply_deployment(
    client: &Client,
    namespace: &str,
    deployment: &Deployment,
    pp: &PatchParams,
) -> Result<(), ReconcileError> {
    let api: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let name = deployment
        .metadata
        .name
        .as_deref()
        .unwrap_or_default()
        .to_string();
    let payload = into_apply_payload("apps/v1", "Deployment", deployment)?;
    api.patch(&name, pp, &Patch::Apply(&payload)).await?;
    Ok(())
}

async fn apply_service(
    client: &Client,
    namespace: &str,
    service: &Service,
    pp: &PatchParams,
) -> Result<(), ReconcileError> {
    let api: Api<Service> = Api::namespaced(client.clone(), namespace);
    let name = service
        .metadata
        .name
        .as_deref()
        .unwrap_or_default()
        .to_string();
    let payload = into_apply_payload("v1", "Service", service)?;
    api.patch(&name, pp, &Patch::Apply(&payload)).await?;
    Ok(())
}

async fn apply_status(
    client: &Client,
    namespace: &str,
    name: &str,
    status: &ApplicationStatus,
    pp: &PatchParams,
) -> Result<(), ReconcileError> {
    let api: Api<Application> = Api::namespaced(client.clone(), namespace);
    let payload = json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "Application",
        "metadata": { "name": name },
        "status": status,
    });
    api.patch_status(name, pp, &Patch::Apply(&payload)).await?;
    Ok(())
}

/// Serialize an arbitrary k8s-openapi type and inject the
/// `apiVersion` + `kind` fields the apiserver requires for SSA.
fn into_apply_payload<T: serde::Serialize>(
    api_version: &str,
    kind: &str,
    obj: &T,
) -> Result<Value, serde_json::Error> {
    let mut value = serde_json::to_value(obj)?;
    if let Value::Object(map) = &mut value {
        map.insert(
            "apiVersion".to_string(),
            Value::String(api_version.to_string()),
        );
        map.insert("kind".to_string(), Value::String(kind.to_string()));
    }
    Ok(value)
}

/// Cluster-internal FQDN for the rendered Service.
fn cluster_internal_endpoint_url(service: &str, namespace: &str, port: i32) -> String {
    format!("http://{service}.{namespace}.svc.cluster.local:{port}")
}

/// Build a fresh `ApplicationStatus` from the Application's
/// observed generation + reconcile result.
fn build_status(
    app: &Application,
    phase: &str,
    conditions: Vec<ApplicationCondition>,
    endpoint_url: Option<String>,
) -> ApplicationStatus {
    ApplicationStatus {
        phase: Some(phase.to_string()),
        observed_generation: app.metadata.generation,
        conditions: Some(conditions),
        endpoint_url,
    }
}

fn ready_condition(
    status: &str,
    reason: &str,
    message: &str,
    previous: &[ApplicationCondition],
) -> ApplicationCondition {
    // Per k8s convention, `lastTransitionTime` moves only when the
    // condition's `status` field changes (False → True etc.). If
    // we just observed a Ready condition with the same `status`
    // value, reuse its timestamp. This is what stops the v0.1.61
    // hot-reconcile loop: identical status output ⇒ SSA patch is
    // a no-op ⇒ no watch event on our own write ⇒ no spurious
    // re-reconcile.
    let last_transition_time = previous
        .iter()
        .find(|c| c.type_ == "Ready" && c.status == status)
        .map(|c| c.last_transition_time.clone())
        .unwrap_or_else(|| Utc::now().to_rfc3339());
    ApplicationCondition {
        type_: "Ready".to_string(),
        status: status.to_string(),
        last_transition_time,
        reason: reason.to_string(),
        message: message.to_string(),
        observed_generation: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use operator_core::ApplicationSpec;

    #[test]
    fn endpoint_url_uses_cluster_local_fqdn() {
        let url = cluster_internal_endpoint_url("web", "default", 80);
        assert_eq!(url, "http://web.default.svc.cluster.local:80");
    }

    #[test]
    fn into_apply_payload_injects_apiversion_and_kind() {
        let value = into_apply_payload(
            "apps/v1",
            "Deployment",
            &serde_json::json!({"metadata": {"name": "x"}}),
        )
        .unwrap();
        let map = value.as_object().unwrap();
        assert_eq!(
            map.get("apiVersion").and_then(|v| v.as_str()),
            Some("apps/v1")
        );
        assert_eq!(map.get("kind").and_then(|v| v.as_str()), Some("Deployment"));
        // The original metadata.name still survives.
        assert_eq!(
            map.get("metadata")
                .and_then(|v| v.get("name"))
                .and_then(|v| v.as_str()),
            Some("x")
        );
    }

    #[test]
    fn build_status_carries_observed_generation_phase_and_endpoint() {
        let mut app = Application::new("web", ApplicationSpec::default());
        app.metadata.generation = Some(7);
        let conds = vec![ready_condition("True", "Ok", "ok", &[])];
        let s = build_status(&app, "Ready", conds, Some("http://x:80".into()));
        assert_eq!(s.phase.as_deref(), Some("Ready"));
        assert_eq!(s.observed_generation, Some(7));
        assert_eq!(s.endpoint_url.as_deref(), Some("http://x:80"));
        let cs = s.conditions.expect("conditions");
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].type_, "Ready");
        assert_eq!(cs[0].status, "True");
    }

    #[test]
    fn ready_condition_preserves_transition_time_when_status_unchanged() {
        // Regression guard for v0.1.63: the operator wrote status
        // on every reconcile, and the previous `ready_condition`
        // always set `Utc::now()` for lastTransitionTime — so each
        // write produced a status-diff, the apiserver fired a
        // watch event on our own update, and the controller looped
        // hot. Per k8s convention `lastTransitionTime` moves only
        // when the condition's `status` value changes. With the
        // fix in place, a second `ready_condition` call carrying
        // the previous condition slice must return the SAME
        // timestamp string.
        let previous = vec![ApplicationCondition {
            type_: "Ready".to_string(),
            status: "True".to_string(),
            last_transition_time: "2026-05-11T00:21:18.194260724+00:00".to_string(),
            reason: "ReconcileSucceeded".to_string(),
            message: "first reconcile".to_string(),
            observed_generation: None,
        }];
        let next = ready_condition(
            "True",
            "ReconcileSucceeded",
            "Reconcile completed; child Deployment and Service applied.",
            &previous,
        );
        assert_eq!(
            next.last_transition_time, previous[0].last_transition_time,
            "lastTransitionTime must be preserved when status is unchanged (k8s convention)"
        );
        assert_eq!(next.status, "True");
    }

    #[test]
    fn ready_condition_bumps_transition_time_when_status_flips() {
        // The other half of the invariant: when status DOES change
        // (True ↔ False), the timestamp MUST move forward so
        // downstream tooling (alertmanager, dashboards, audit
        // logs) sees a real transition event.
        let previous = vec![ApplicationCondition {
            type_: "Ready".to_string(),
            status: "False".to_string(),
            last_transition_time: "2026-05-11T00:00:00+00:00".to_string(),
            reason: "ApplyFailed".to_string(),
            message: "old".to_string(),
            observed_generation: None,
        }];
        let next = ready_condition("True", "ReconcileSucceeded", "now ok", &previous);
        assert_ne!(
            next.last_transition_time, previous[0].last_transition_time,
            "lastTransitionTime must change when status flips False → True"
        );
        assert_eq!(next.status, "True");
    }

    #[test]
    fn ready_condition_has_rfc3339_timestamp_and_required_fields() {
        let c = ready_condition(
            "False",
            "ApplyFailed",
            "the deployment apply step returned an error",
            &[],
        );
        assert_eq!(c.type_, "Ready");
        assert_eq!(c.status, "False");
        assert_eq!(c.reason, "ApplyFailed");
        assert!(c.message.contains("apply step"));
        // RFC3339 timestamps look like `2026-05-08T12:34:56.789+00:00`
        // — assert the structural anchors.
        assert!(
            c.last_transition_time.contains('T'),
            "{}",
            c.last_transition_time
        );
        assert!(
            c.last_transition_time.ends_with("+00:00") || c.last_transition_time.ends_with('Z'),
            "{}",
            c.last_transition_time
        );
    }
}
