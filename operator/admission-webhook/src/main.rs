// SPDX-License-Identifier: FSL-1.1-MIT
//! AppRafter admission webhook binary.
//!
//! Reads `PORT` (default 8443), `TLS_CERT_PATH` (default
//! `/tls/tls.crt`), and `TLS_KEY_PATH` (default `/tls/tls.key`)
//! from the environment. If both cert + key files exist, serves
//! HTTPS via axum-server + rustls; otherwise falls back to plain
//! HTTP (useful for `cargo run` during development).

use std::env;
use std::net::SocketAddr;
use std::path::Path;

use admission_webhook::build_router;
use axum_server::tls_rustls::RustlsConfig;
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

    let cert_path = env::var("TLS_CERT_PATH").unwrap_or_else(|_| "/tls/tls.crt".into());
    let key_path = env::var("TLS_KEY_PATH").unwrap_or_else(|_| "/tls/tls.key".into());

    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    if Path::new(&cert_path).exists() && Path::new(&key_path).exists() {
        info!(?addr, %cert_path, %key_path, "admission-webhook listening with TLS");
        let config = RustlsConfig::from_pem_file(&cert_path, &key_path).await?;
        axum_server::bind_rustls(addr, config)
            .serve(build_router().into_make_service())
            .await?;
    } else {
        info!(?addr, "admission-webhook listening (HTTP — TLS files not found)");
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, build_router()).await?;
    }

    Ok(())
}
