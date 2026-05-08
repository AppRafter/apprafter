// SPDX-License-Identifier: FSL-1.1-MIT
//! AppRafter admission webhook binary.
//!
//! Reads `PORT` (default 8443) from the environment, builds the axum
//! router from `admission_webhook::build_router`, and serves it.
//!
//! v0.1.23 listens on plain HTTP. TLS termination via the
//! cert-manager-issued Secret lands in v0.1.24 once the Deployment
//! manifest mounts it.

use std::env;
use std::net::SocketAddr;

use admission_webhook::build_router;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let port: u16 = env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8443);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!(?addr, "admission-webhook listening");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, build_router()).await?;

    Ok(())
}
