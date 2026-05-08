// SPDX-License-Identifier: FSL-1.1-MIT
//! AppRafter operator binary.
//!
//! Spawns:
//!   - the axum HTTP server (`/healthz` + `/readyz` + `/metrics`)
//!     on `HTTP_PORT` (default 8080);
//!   - the `Application` Controller against a `kube::Client`
//!     resolved via `Client::try_default()` (in-cluster config or
//!     `~/.kube/config` fallback).
//!
//! Either task exiting (or ctrl-c) tears the whole process down.

use std::env;
use std::net::SocketAddr;
use std::sync::Arc;

use apprafter_operator::build_router;
use kube::Client;
use operator_controllers_application as application_controller;
use operator_core::Metrics;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

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

    let controller_handle = tokio::spawn({
        let client = client.clone();
        let metrics = metrics.clone();
        async move {
            if let Err(err) = application_controller::run(client, metrics).await {
                error!(%err, "Application controller error");
            }
        }
    });

    tokio::select! {
        _ = server_handle => warn!("HTTP server exited"),
        _ = controller_handle => warn!("Application controller exited"),
        _ = tokio::signal::ctrl_c() => info!("ctrl-c received, shutting down"),
    }

    Ok(())
}
