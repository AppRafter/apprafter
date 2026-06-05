// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Real-cluster smoke for the ResourceClaim provisioner. Gated by
//! `APPRAFTER_K8S_SMOKE=1` and `#[ignore]` so the standard `cargo test`
//! run does NOT need a live cluster.
//!
//! Invocation (from a `nix develop` shell with a kubeconfig pointing at
//! a cluster running the AppRafter operator — scheduler + provisioner —
//! plus the CNPG operator + CRDs from the `cloudnative-pg` component):
//!
//!   APPRAFTER_K8S_SMOKE=1 KUBECONFIG=… \
//!   cargo test \
//!     -p operator-controllers-resourceclaim-provisioner \
//!     --test cluster_smoke_test \
//!     -- --ignored
//!
//! Per the AppRafter feedback memory, environment-conditioned silent skips
//! are forbidden — when the gate variable is unset and the test runs with
//! `--ignored`, it panics rather than returning quietly.
//!
//! Flow: seed a `pg-integrated` ServiceProvider (type pg, backend
//! cloudnative-pg, label tier=integrated) in `apprafter-system` and a
//! matching `ResourceClaim` (type pg, selector {tier: integrated}) in a
//! temp namespace. The in-cluster scheduler marks it `Scheduled=True`;
//! the provisioner then provisions it. Assert the claim flips
//! `status.ready=true` with a `connectionSecretRef`, the named connection
//! Secret carries `DATABASE_URL`, and a CNPG `Database` CR appears in the
//! cnpg-system namespace.

use std::collections::BTreeMap;
use std::time::Duration;

use k8s_openapi::api::core::v1::{Namespace, Secret};
use kube::api::{Api, ApiResource, DeleteParams, DynamicObject, ObjectMeta, PostParams};
use kube::core::GroupVersionKind;
use kube::Client;
use operator_core::{ResourceClaim, ResourceClaimSpec, ServiceProvider, ServiceProviderSpec};
use serde_json::json;

/// Poll the claim until `status.ready == true`, returning the
/// `connectionSecretRef`, or `None` on timeout.
async fn poll_claim_ready(
    api: &Api<ResourceClaim>,
    name: &str,
    timeout: Duration,
) -> Option<String> {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if let Ok(claim) = api.get(name).await {
            if let Some(status) = &claim.status {
                if status.ready == Some(true) {
                    return status.connection_secret_ref.clone();
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    None
}

#[tokio::test]
#[ignore = "real cluster — set APPRAFTER_K8S_SMOKE=1 + KUBECONFIG to run"]
async fn provisioner_provisions_a_scheduled_pg_claim() {
    if std::env::var("APPRAFTER_K8S_SMOKE").as_deref() != Ok("1") {
        panic!(
            "this test requires APPRAFTER_K8S_SMOKE=1 (and a KUBECONFIG pointing at a \
             cluster running the AppRafter operator + CNPG operator and CRDs)"
        );
    }

    let client = Client::try_default()
        .await
        .expect("kube::Client::try_default() — check KUBECONFIG");

    // 1. Temp namespace for the claim.
    let ns_name = "apprafter-smoke-prov";
    let ns_api: Api<Namespace> = Api::all(client.clone());
    let _ = ns_api.delete(ns_name, &DeleteParams::default()).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    ns_api
        .create(
            &PostParams::default(),
            &Namespace {
                metadata: ObjectMeta {
                    name: Some(ns_name.to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .expect("create temp namespace");

    // 2. Seed the pg ServiceProvider in apprafter-system (idempotent).
    let sp_api: Api<ServiceProvider> = Api::namespaced(client.clone(), "apprafter-system");
    let provider = ServiceProvider {
        metadata: ObjectMeta {
            name: Some("pg-smoke".to_string()),
            namespace: Some("apprafter-system".to_string()),
            labels: Some(BTreeMap::from([(
                "tier".to_string(),
                "integrated".to_string(),
            )])),
            ..Default::default()
        },
        spec: ServiceProviderSpec {
            type_: "pg".to_string(),
            backend: "cloudnative-pg".to_string(),
            config: Some(json!({
                "cluster": "platform-postgres",
                "namespace": "cnpg-system",
                "instances": 1,
                "storage": "1Gi",
            })),
        },
        status: None,
    };
    let _ = sp_api.delete("pg-smoke", &DeleteParams::default()).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    sp_api
        .create(&PostParams::default(), &provider)
        .await
        .expect("create pg-smoke ServiceProvider");

    // 3. Apply a matching ResourceClaim.
    let claim_api: Api<ResourceClaim> = Api::namespaced(client.clone(), ns_name);
    let claim = ResourceClaim {
        metadata: ObjectMeta {
            name: Some("pg-claim".to_string()),
            namespace: Some(ns_name.to_string()),
            ..Default::default()
        },
        spec: ResourceClaimSpec {
            type_: "pg".to_string(),
            selector: BTreeMap::from([("tier".to_string(), "integrated".to_string())]),
            size: None,
            persistent: None,
        },
        status: None,
    };
    claim_api
        .create(&PostParams::default(), &claim)
        .await
        .expect("create ResourceClaim");

    // 4. Poll until provisioned (scheduler + provisioner; CNPG cluster
    //    creation + first-instance bootstrap dominates — allow 5 min).
    let secret_ref = poll_claim_ready(&claim_api, "pg-claim", Duration::from_secs(300)).await;
    let secret_ref =
        secret_ref.expect("claim must flip status.ready=true with a connectionSecretRef");

    // 5. The connection Secret carries DATABASE_URL.
    let secret_api: Api<Secret> = Api::namespaced(client.clone(), ns_name);
    let secret = secret_api
        .get(&secret_ref)
        .await
        .expect("connection Secret must exist");
    let data = secret.data.unwrap_or_default();
    assert!(
        data.contains_key("DATABASE_URL"),
        "connection Secret must carry DATABASE_URL"
    );

    // 6. A CNPG Database CR appears in cnpg-system for the claim. Its
    //    metadata.name is the DNS-1123 object name (`claim-...`, dashes) —
    //    NOT the underscore Postgres identifier used inside Postgres.
    let db_ar = ApiResource::from_gvk(&GroupVersionKind::gvk(
        "postgresql.cnpg.io",
        "v1",
        "Database",
    ));
    let db_api: Api<DynamicObject> = Api::namespaced_with(client.clone(), "cnpg-system", &db_ar);
    let databases = db_api
        .list(&Default::default())
        .await
        .expect("list CNPG Databases");
    assert!(
        databases.items.iter().any(|d| d
            .metadata
            .name
            .as_deref()
            .map(|n| n.starts_with("claim-"))
            .unwrap_or(false)),
        "a CNPG Database for the claim must be applied"
    );

    // 7. Cleanup.
    let _ = ns_api.delete(ns_name, &DeleteParams::default()).await;
    let _ = sp_api.delete("pg-smoke", &DeleteParams::default()).await;
}
