// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Real-cluster smoke for the ResourceClaim scheduler. Gated by
//! `APPRAFTER_K8S_SMOKE=1` and `#[ignore]` so the standard
//! `cargo test` run does NOT need a live cluster.
//!
//! Invocation (from a `nix develop` shell with a kubeconfig pointing at
//! a cluster with the AppRafter operator and CRDs installed):
//!
//!   APPRAFTER_K8S_SMOKE=1 KUBECONFIG=… \
//!   cargo test \
//!     -p operator-controllers-resourceclaim-scheduler \
//!     --test scheduler_smoke_test \
//!     -- --ignored
//!
//! Per the AppRafter feedback memory, environment-conditioned silent skips
//! are forbidden — when the gate variable is unset and the test runs with
//! `--ignored`, it panics rather than returning quietly.

use std::collections::BTreeMap;
use std::time::Duration;

use k8s_openapi::api::core::v1::Namespace;
use kube::api::{Api, DeleteParams, ObjectMeta, PostParams};
use kube::Client;
use operator_core::{ResourceClaim, ResourceClaimSpec, ServiceProvider, ServiceProviderSpec};
/// Helper: poll `claim.status.provider` until set or timeout.
async fn poll_claim_provider(
    api: &Api<ResourceClaim>,
    name: &str,
    timeout: Duration,
) -> Option<String> {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if let Ok(claim) = api.get(name).await {
            if let Some(status) = &claim.status {
                if let Some(provider) = &status.provider {
                    return Some(provider.clone());
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    None
}

/// Helper: poll `claim.status.conditions` for a condition with the given
/// type and status value (e.g. `Scheduled`/`False`), until set or timeout.
async fn poll_claim_condition(
    api: &Api<ResourceClaim>,
    name: &str,
    cond_type: &str,
    cond_status: &str,
    timeout: Duration,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if let Ok(claim) = api.get(name).await {
            if let Some(status) = &claim.status {
                if let Some(conditions) = &status.conditions {
                    if conditions
                        .iter()
                        .any(|c| c.type_ == cond_type && c.status == cond_status)
                    {
                        return true;
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    false
}

/// Happy-path: two `pg` ServiceProviders (`pg-bravo`, `pg-alpha`) with
/// label `tier=integrated` in `apprafter-system`; one `ResourceClaim`
/// with `type: pg` + `selector: {tier: integrated}` in a temp namespace.
///
/// Expected: scheduler picks `pg-alpha` (alphabetically first) and sets
/// `status.provider = "pg-alpha"` + a `Scheduled=True` condition.
///
/// No-match path: a second claim with `selector: {tier: nonexistent}`
/// should get `Scheduled=False` (provider absent).
#[tokio::test]
#[ignore = "real cluster — set APPRAFTER_K8S_SMOKE=1 + KUBECONFIG to run"]
async fn scheduler_matches_claim_to_alphabetically_first_provider() {
    if std::env::var("APPRAFTER_K8S_SMOKE").as_deref() != Ok("1") {
        panic!(
            "this test requires APPRAFTER_K8S_SMOKE=1 (and a KUBECONFIG pointing at a \
             cluster with the AppRafter CRDs and operator installed)"
        );
    }

    let client = Client::try_default()
        .await
        .expect("kube::Client::try_default() — check KUBECONFIG");

    // -----------------------------------------------------------------------
    // 1. Create a temp namespace for the ResourceClaims.
    // -----------------------------------------------------------------------
    let ns_name = "apprafter-smoke-sched";
    let ns_api: Api<Namespace> = Api::all(client.clone());
    let ns_obj = Namespace {
        metadata: ObjectMeta {
            name: Some(ns_name.to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    // Best-effort delete in case a previous run left it behind.
    let _ = ns_api.delete(ns_name, &DeleteParams::default()).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    ns_api
        .create(&PostParams::default(), &ns_obj)
        .await
        .expect("create temp namespace");

    // -----------------------------------------------------------------------
    // 2. Apply two ServiceProviders in apprafter-system.
    //    pg-bravo and pg-alpha — scheduler must pick pg-alpha (alphabetically
    //    first among same-type, same-label providers).
    // -----------------------------------------------------------------------
    let sp_api: Api<ServiceProvider> = Api::namespaced(client.clone(), "apprafter-system");

    let make_provider = |name: &str| ServiceProvider {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
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
            config: None,
        },
        status: None,
    };

    // Best-effort cleanup of previous smoke runs.
    let _ = sp_api.delete("pg-bravo", &DeleteParams::default()).await;
    let _ = sp_api.delete("pg-alpha", &DeleteParams::default()).await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    sp_api
        .create(&PostParams::default(), &make_provider("pg-bravo"))
        .await
        .expect("create pg-bravo ServiceProvider");
    sp_api
        .create(&PostParams::default(), &make_provider("pg-alpha"))
        .await
        .expect("create pg-alpha ServiceProvider");

    // -----------------------------------------------------------------------
    // 3. Apply a matching ResourceClaim in the temp namespace.
    // -----------------------------------------------------------------------
    let claim_api: Api<ResourceClaim> = Api::namespaced(client.clone(), ns_name);
    let match_claim = ResourceClaim {
        metadata: ObjectMeta {
            name: Some("pg-match".to_string()),
            namespace: Some(ns_name.to_string()),
            ..Default::default()
        },
        spec: ResourceClaimSpec {
            type_: "pg".to_string(),
            selector: BTreeMap::from([("tier".to_string(), "integrated".to_string())]),
            size: None,
        },
        status: None,
    };
    claim_api
        .create(&PostParams::default(), &match_claim)
        .await
        .expect("create matching ResourceClaim");

    // -----------------------------------------------------------------------
    // 4. Poll until provider is set (timeout 30 s).
    // -----------------------------------------------------------------------
    let chosen = poll_claim_provider(&claim_api, "pg-match", Duration::from_secs(30)).await;
    assert_eq!(
        chosen.as_deref(),
        Some("pg-alpha"),
        "scheduler must pick the alphabetically-first matching provider"
    );

    // Also verify Scheduled=True condition is present.
    let scheduled_true = poll_claim_condition(
        &claim_api,
        "pg-match",
        "Scheduled",
        "True",
        Duration::from_secs(5),
    )
    .await;
    assert!(
        scheduled_true,
        "Scheduled=True condition must be set after a successful match"
    );

    // -----------------------------------------------------------------------
    // 5. Apply a no-match ResourceClaim (nonexistent selector label value).
    // -----------------------------------------------------------------------
    let no_match_claim = ResourceClaim {
        metadata: ObjectMeta {
            name: Some("pg-no-match".to_string()),
            namespace: Some(ns_name.to_string()),
            ..Default::default()
        },
        spec: ResourceClaimSpec {
            type_: "pg".to_string(),
            selector: BTreeMap::from([("tier".to_string(), "nonexistent".to_string())]),
            size: None,
        },
        status: None,
    };
    claim_api
        .create(&PostParams::default(), &no_match_claim)
        .await
        .expect("create no-match ResourceClaim");

    // Poll until Scheduled=False is set (timeout 30 s).
    let scheduled_false = poll_claim_condition(
        &claim_api,
        "pg-no-match",
        "Scheduled",
        "False",
        Duration::from_secs(30),
    )
    .await;
    assert!(
        scheduled_false,
        "Scheduled=False condition must be set when no provider matches"
    );

    // provider must remain unset for the no-match claim.
    let no_match_obj = claim_api
        .get("pg-no-match")
        .await
        .expect("get no-match claim");
    assert!(
        no_match_obj
            .status
            .as_ref()
            .and_then(|s| s.provider.as_ref())
            .is_none(),
        "status.provider must be absent for a no-match claim"
    );

    // -----------------------------------------------------------------------
    // 6. Cleanup: delete temp namespace + providers.
    // -----------------------------------------------------------------------
    let _ = ns_api.delete(ns_name, &DeleteParams::default()).await;
    let _ = sp_api.delete("pg-bravo", &DeleteParams::default()).await;
    let _ = sp_api.delete("pg-alpha", &DeleteParams::default()).await;
}
