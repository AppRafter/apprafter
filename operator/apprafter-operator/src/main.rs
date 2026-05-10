// SPDX-License-Identifier: FSL-1.1-MIT
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
use kube::Client;
use operator_controllers_application as application_controller;
use operator_core::{LeaderConfig, LeaderElection, Metrics};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

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

    let env_name = env::var("APPRAFTER_ENV").ok().filter(|v| !v.is_empty());
    if let Some(name) = &env_name {
        info!(env = %name, "active environment selected");
    }
    let controller_handle = tokio::spawn({
        let client = client.clone();
        let metrics = metrics.clone();
        let env_name = env_name.clone();
        async move {
            if let Err(err) = application_controller::run(client, metrics, env_name).await {
                error!(%err, "Application controller error");
            }
        }
    });

    tokio::select! {
        _ = server_handle => warn!("HTTP server exited"),
        _ = controller_handle => warn!("Application controller exited"),
        _ = leader_handle => warn!("leader election exited"),
        _ = tokio::signal::ctrl_c() => info!("ctrl-c received, shutting down"),
    }

    Ok(())
}
