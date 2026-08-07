// SPDX-License-Identifier: FSL-1.1-Apache-2.0
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

use admission_webhook::{build_router, install_rustls_crypto_provider, operator_service_account};
use axum_server::tls_rustls::RustlsConfig;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Must come before any rustls-using API (axum-server's
    // bind_rustls / RustlsConfig::from_pem_file). rustls 0.23+
    // requires an explicit CryptoProvider when multiple
    // backends are linked.
    install_rustls_crypto_provider();

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

    // 2.16b-sec (F-1): resolve the operator ServiceAccount ONCE at startup
    // (from `OPERATOR_SERVICEACCOUNT`, injected by the Helm chart on the
    // release namespace; falls back to the hardcoded default when unset). The
    // status guards + identity gates all read this single value. Priming +
    // logging it here surfaces a mis-set env in the pod log rather than at
    // first admission.
    let operator_sa = operator_service_account();
    info!(%operator_sa, "resolved operator ServiceAccount for status/identity guards");

    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    if Path::new(&cert_path).exists() && Path::new(&key_path).exists() {
        info!(?addr, %cert_path, %key_path, "admission-webhook listening with TLS");
        let config = RustlsConfig::from_pem_file(&cert_path, &key_path).await?;
        axum_server::bind_rustls(addr, config)
            .serve(build_router().into_make_service())
            .await?;
    } else {
        info!(
            ?addr,
            "admission-webhook listening (HTTP — TLS files not found)"
        );
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, build_router()).await?;
    }

    Ok(())
}
