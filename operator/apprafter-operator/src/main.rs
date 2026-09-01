// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! AppRafter operator binary.
//!
//! Spawns:
//!   - the axum HTTP server (`/healthz` + `/readyz` + `/metrics`)
//!     on `HTTP_PORT` (default 8080) — runs unconditionally so the
//!     pod's probes succeed even before leadership is acquired;
//!   - a Lease-based leader election loop (`operator-core::leader`)
//!     against `apprafter-system/apprafter-operator`;
//!   - the `Application` Controller — but only after we hold the
//!     Lease.
//!
//! Any task exiting (HTTP server crash, controller stream end,
//! leader-loss after 3 consecutive renewal failures, ctrl-c) tears
//! the whole process down so the Deployment restart picks up.

use std::env;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use apprafter_operator::{build_router, install_rustls_crypto_provider};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::api::Api;
use kube::Client;
use operator_controllers_application as application_controller;
use operator_core::{LeaderConfig, LeaderElection, Metrics};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

/// Probe whether the `ciliumnetworkpolicies.cilium.io` CRD is served on
/// this cluster (2.10 / ADR 0045). Run ONCE at startup and threaded into
/// the Application controller's `Context` so the reconcile loop can gate
/// the egress-CNP SSA apply: on a non-Cilium cluster (e2e / kindnet) the
/// CRD is unserved and applying a `CiliumNetworkPolicy` 404s every
/// reconcile. Best-effort: any read error degrades to `false` (skip the
/// apply) rather than crash-looping the operator.
async fn cilium_available(client: &Client) -> bool {
    let api: Api<CustomResourceDefinition> = Api::all(client.clone());
    api.get_opt("ciliumnetworkpolicies.cilium.io")
        .await
        .ok()
        .flatten()
        .is_some()
}

/// Probe whether the `httproutes.gateway.networking.k8s.io` CRD is served on
/// this cluster (1.83b). Run ONCE at startup and threaded into the Application
/// controller's `Context` so the reconcile loop can gate the HTTPRoute SSA
/// apply: on a cluster without the Gateway-API CRDs (e.g. a plain e2e/kindnet
/// cluster) applying an HTTPRoute would 404 every reconcile. Best-effort: any
/// read error degrades to `false` (skip the apply).
async fn gateway_api_available(client: &Client) -> bool {
    let api: Api<CustomResourceDefinition> = Api::all(client.clone());
    api.get_opt("httproutes.gateway.networking.k8s.io")
        .await
        .ok()
        .flatten()
        .is_some()
}

/// Probe whether the `verticalpodautoscalers.autoscaling.k8s.io` CRD is served
/// on this cluster (2.16e / ADR 0054). Run ONCE at startup and threaded into
/// the Application controller's `Context` so the reconcile loop can gate the
/// VPA SSA apply: on a cluster without the VPA operator the CRD is unserved and
/// applying a `VerticalPodAutoscaler` 404s every reconcile. Best-effort: any
/// read error degrades to `false` (skip the apply).
async fn vpa_available(client: &Client) -> bool {
    let api: Api<CustomResourceDefinition> = Api::all(client.clone());
    api.get_opt("verticalpodautoscalers.autoscaling.k8s.io")
        .await
        .ok()
        .flatten()
        .is_some()
}

/// Probe whether this cluster's Kubernetes can actuate an in-place pod
/// resize (2.22d / D10). Run ONCE at startup and threaded into the
/// Application controller, which folds a negative into the same
/// `status.recommendedResources.notApplied` message a blocked resize uses.
///
/// In-place resize is a Kubernetes feature, not a VPA one, and nothing pins
/// the Kubernetes version — `build_k3s_user_data` installs the stable
/// channel. It works today because the gate is on by default at the versions
/// that channel serves, which is an upstream default rather than something
/// the platform arranges. Without this probe, a cluster that drifted below
/// the threshold would run `updateMode: InPlace` forever with nothing
/// actuating and every gate reporting green.
///
/// Unreadable version → `true`: refusing to claim a defect we cannot
/// evidence. A wrong "your cluster is too old" on every app would be worse
/// than the silence, and the pod-condition probe still covers the real
/// blocks.
async fn in_place_resize_supported(client: &Client) -> bool {
    let Ok(info) = client.apiserver_version().await else {
        return true;
    };
    match application_controller::parse_apiserver_version(&info.major, &info.minor) {
        Some((major, minor)) => application_controller::in_place_resize_supported(major, minor),
        None => true,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // Must run BEFORE any rustls-using API (kube::Client::try_default)
    // — rustls 0.23+ panics if no process-level CryptoProvider is
    // installed. See `apprafter_operator::install_rustls_crypto_provider`
    // for the full rationale + the regression-guard tests.
    install_rustls_crypto_provider();

    let metrics = Arc::new(Metrics::new());
    let client = Client::try_default().await?;

    let port: u16 = env::var("HTTP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!(?addr, "apprafter-operator HTTP listening");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let router = build_router(metrics.clone());

    let server_handle = tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, router).await {
            error!(%err, "HTTP server error");
        }
    });

    // Leader election — derive holder id from POD_NAME (set by the
    // downward API in the Helm chart, v0.1.29) or fall back to a
    // process-id stamp for local runs.
    let holder_id =
        env::var("POD_NAME").unwrap_or_else(|_| format!("local-{}", std::process::id()));
    let leader = LeaderElection::new(
        client.clone(),
        LeaderConfig::for_apprafter_operator(&holder_id),
    );
    let is_leader = leader.is_leader_handle();

    let leader_handle = tokio::spawn(async move {
        if let Err(err) = leader.run().await {
            error!(%err, "leader election exited");
        }
    });

    info!(holder = %holder_id, "waiting for leadership");
    while !is_leader.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    info!("leadership acquired — starting Application controller");

    // 2.10 (ADR 0045): probe ONCE whether Cilium's CNP CRD is served, and
    // thread the result into the Application controller. On a non-Cilium
    // cluster (e2e / kindnet) the operator renders the egress CNP but skips
    // the apply (it would 404 every reconcile).
    let cilium = cilium_available(&client).await;
    info!(
        cilium_available = cilium,
        "probed Cilium CNP CRD availability"
    );

    let gateway_api = gateway_api_available(&client).await;
    info!(
        gateway_api_available = gateway_api,
        "probed Gateway-API HTTPRoute CRD availability"
    );

    // 2.16e (ADR 0054): probe ONCE whether the VPA CRD is served, and thread
    // the result into the Application controller. On a cluster without the VPA
    // operator the `verticalpodautoscalers.autoscaling.k8s.io` CRD is absent
    // and applying a VPA would 404 every reconcile.
    // Seed the flag with a startup probe; the Application controller lazily
    // re-probes while `false` (the startup probe can lose the race with the VPA
    // component's CRD install), so an `AtomicBool` shared into the reconcile.
    let vpa = Arc::new(std::sync::atomic::AtomicBool::new(
        vpa_available(&client).await,
    ));
    info!(
        vpa_available = vpa.load(Ordering::Relaxed),
        "probed VerticalPodAutoscaler CRD availability (lazily re-probed if false)"
    );

    // 2.22d (D10): probe the KUBERNETES half of right-sizing. The VPA CRD
    // being served says the recommender can run; it says nothing about
    // whether the kubelet can act on a recommendation without a restart.
    let in_place = in_place_resize_supported(&client).await;
    info!(
        in_place_resize_supported = in_place,
        "probed in-place pod resize support (updateMode: InPlace actuates nothing without it)"
    );

    // 2.22h / D16: how long a recorded reconcile failure stays on the object
    // after its last sighting. Overridable so a walk can prove ageing without
    // waiting an hour — the `APPRAFTER_REAP_DWELL_SECS` pattern. LOGGED, so
    // "aged out" and "was never written" are distinguishable, which is the
    // same class of ambiguity this whole subphase exists to end.
    let problem_ttl_secs: i64 = env::var("APPRAFTER_PROBLEM_TTL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(operator_core::problems::DEFAULT_PROBLEM_TTL_SECS);
    // Same knob class, same reason: at the shipped 900s floor a walk cannot
    // observe `count` accumulate without sampling for a quarter of an hour.
    let problem_refresh_floor_secs: i64 = env::var("APPRAFTER_PROBLEM_REFRESH_FLOOR_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(operator_core::problems::PROBLEM_REFRESH_FLOOR_SECS);
    info!(
        problem_ttl_secs,
        problem_refresh_floor_secs, "recent-problem retention configured (2.22h / D16)"
    );

    // 2.9 (ADR 0044): the active environment is now a PER-CR property
    // (`Application.spec.environment`), resolved inside the reconcile
    // loop — there is no cluster-wide `APPRAFTER_ENV` selector anymore.
    let controller_handle = tokio::spawn({
        let client = client.clone();
        let metrics = metrics.clone();
        async move {
            if let Err(err) = application_controller::run(
                client,
                metrics,
                cilium,
                gateway_api,
                vpa,
                in_place,
                operator_core::problems::ProblemTuning {
                    ttl_secs: problem_ttl_secs,
                    refresh_floor_secs: problem_refresh_floor_secs,
                },
            )
            .await
            {
                error!(%err, "Application controller error");
            }
        }
    });

    // PlatformController — peer to the Application controller in
    // the same binary per Track B.1.73 design. Both run after the
    // single Lease is held, so a lease loss tears the process
    // down and both controllers go with it.
    let platform_controller_handle = tokio::spawn({
        let client = client.clone();
        let metrics = metrics.clone();
        async move {
            if let Err(err) = operator_controllers_platform_stack::run(client, metrics).await {
                error!(%err, "PlatformController error");
            }
        }
    });

    // MigrationController — third controller (Track B.1.76).
    // Owns `MigrationPlan.status.*` writes under field manager
    // `migration-controller`. Currently does not consume the
    // shared `metrics` handle — strategy actions are no-ops in
    // 1.76, no counters to surface yet. Wires in when 1.77 +
    // 1.78 ship real action runners.
    let migration_controller_handle = tokio::spawn({
        let client = client.clone();
        async move {
            if let Err(err) = operator_controllers_migration::run(client).await {
                error!(%err, "MigrationController error");
            }
        }
    });

    // SourceCredentialController — fourth controller (1.79c / ADR
    // 0039). Derives Argo `repo-creds` (git half) and workload
    // pull-secrets (registry half) from sealed material, under
    // field manager `apprafter-sourcecredential`, and reports
    // per-half status.
    let sourcecred_controller_handle = tokio::spawn({
        let client = client.clone();
        let metrics = metrics.clone();
        async move {
            if let Err(err) = operator_controllers_sourcecredential::run(client, metrics).await {
                error!(%err, "SourceCredentialController error");
            }
        }
    });

    // ResourceClaimScheduler — fifth controller (Phase 2.3). Matches
    // each ResourceClaim to a ServiceProvider by type + label superset
    // and records the winner in status.provider. Provisioning is 2.4.
    let resourceclaim_scheduler_handle = tokio::spawn({
        let client = client.clone();
        let metrics = metrics.clone();
        async move {
            if let Err(err) =
                operator_controllers_resourceclaim_scheduler::run(client, metrics).await
            {
                error!(%err, "ResourceClaimScheduler controller error");
            }
        }
    });

    // ResourceClaimProvisioner — sixth controller (Phase 2.4c). Picks up
    // each ResourceClaim the scheduler marked Scheduled=True and
    // provisions it into the matched backend: for `cloudnative-pg` it
    // lazily creates the shared `platform-postgres` CNPG Cluster, a
    // per-claim role + database + connection Secret, and writes
    // status.ready / connectionSecretRef / Ready under its own field
    // manager `resourceclaim-provisioner` (never touching the
    // scheduler-owned status.provider / Scheduled).
    // ADR 0042 §10: one `acl_dirty` signal shared by the three tasks that
    // change the set of live Dragonfly ACL users. Each `run*` builds its own
    // Context, so a per-context Notify would be a signal nobody hears — it is
    // created here and threaded in. A poke, never content: the resync loop
    // stays the file's SOLE writer and re-derives from a fresh LIST.
    let acl_dirty = std::sync::Arc::new(tokio::sync::Notify::new());

    let resourceclaim_provisioner_handle = tokio::spawn({
        let client = client.clone();
        let metrics = metrics.clone();
        let acl_dirty = acl_dirty.clone();
        async move {
            if let Err(err) =
                operator_controllers_resourceclaim_provisioner::run(client, metrics, acl_dirty)
                    .await
            {
                error!(%err, "ResourceClaimProvisioner controller error");
            }
        }
    });

    // ResourceClaimGC — seventh controller (Phase 2.4f). Watches
    // RetainedClaim snapshots cluster-wide (the provisioner finalizer
    // writes one into apprafter-system when a pg ResourceClaim is
    // deleted) and, once `spec.retainUntil` (deletion + 7-day grace)
    // passes, drops the per-claim Postgres role (RMW the shared
    // Cluster's spec.managed.roles), the Database (spec.ensure:absent —
    // CNPG drops the DB), the password Secret, and the snapshot. Every
    // step idempotent + 404-tolerant. Lives in the same crate as the
    // provisioner — no Cargo.toml member edit.
    let resourceclaim_gc_handle = tokio::spawn({
        let client = client.clone();
        let metrics = metrics.clone();
        let acl_dirty = acl_dirty.clone();
        async move {
            if let Err(err) =
                operator_controllers_resourceclaim_provisioner::gc::run(client, metrics, acl_dirty)
                    .await
            {
                error!(%err, "ResourceClaimGC controller error");
            }
        }
    });

    // DragonflyAclReconcile — periodic re-pin loop (Phase 2.6-5). Per-claim
    // `$N` ACL users are runtime state on a Dragonfly instance, wiped on a
    // pod restart; this loop re-asserts every live ready dragonfly claim's
    // user on a 300s tick (idempotent ACL SETUSER, password recovered from
    // the claim's connection-Secret DSN) so an app reconnects without
    // WRONGPASS/NOPERM after the instance churns. Same crate as the
    // provisioner — no Cargo.toml member edit.
    let dragonfly_acl_handle = tokio::spawn({
        let client = client.clone();
        let metrics = metrics.clone();
        let acl_dirty = acl_dirty.clone();
        async move {
            if let Err(err) = operator_controllers_resourceclaim_provisioner::run_acl_reconcile(
                client, metrics, acl_dirty,
            )
            .await
            {
                error!(%err, "DragonflyAclReconcile loop error");
            }
        }
    });

    // SharedBackendReaper — ninth loop (ADR 0042 §9). Deletes a Dragonfly
    // pool instance or the shared CNPG cluster once nothing references it.
    // `APPRAFTER_REAP_DWELL_SECS` overrides the default 10-minute dwell so
    // e2e walks can assert a terminal outcome instead of racing a real
    // ten-minute timer; production runs unset, at the default.
    let reap_dwell = std::time::Duration::from_secs(
        env::var("APPRAFTER_REAP_DWELL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(600),
    );
    let reaper_handle = tokio::spawn({
        let client = client.clone();
        let metrics = metrics.clone();
        async move {
            if let Err(err) = operator_controllers_resourceclaim_provisioner::run_reaper(
                client, metrics, reap_dwell,
            )
            .await
            {
                error!(%err, "SharedBackendReaper loop error");
            }
        }
    });

    tokio::select! {
        _ = server_handle => warn!("HTTP server exited"),
        _ = controller_handle => warn!("Application controller exited"),
        _ = platform_controller_handle => warn!("PlatformController exited"),
        _ = migration_controller_handle => warn!("MigrationController exited"),
        _ = sourcecred_controller_handle => warn!("SourceCredentialController exited"),
        _ = resourceclaim_scheduler_handle => warn!("ResourceClaimScheduler controller exited"),
        _ = resourceclaim_provisioner_handle => warn!("ResourceClaimProvisioner controller exited"),
        _ = resourceclaim_gc_handle => warn!("ResourceClaimGC controller exited"),
        _ = dragonfly_acl_handle => warn!("DragonflyAclReconcile loop exited"),
        // Spawned after the leadership-acquired wait loop above, same as
        // every other handle in this select! — leader-gated for free, no
        // extra lease logic of its own. Without this arm the reaper task's
        // death would be invisible: the process would carry on serving
        // /healthz forever with the reaper silently gone.
        _ = reaper_handle => warn!("SharedBackendReaper loop exited"),
        _ = leader_handle => warn!("leader election exited"),
        _ = tokio::signal::ctrl_c() => info!("ctrl-c received, shutting down"),
    }

    Ok(())
}
