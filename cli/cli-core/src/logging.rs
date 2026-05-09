// SPDX-License-Identifier: FSL-1.1-MIT
//! Structured logging for `platform-cli`.
//!
//! Honours `RUST_LOG`. Default level is `info` for the CLI crates
//! and `warn` for everything else. Calling [`init`] more than once
//! is harmless: the second call is a no-op.
//!
//! Writes to **stderr**, not stdout — so commands whose stdout is
//! piped into another process (e.g. `kubeconfig | KUBECONFIG=/dev/stdin
//! kubectl …`, `kubeconfig > /tmp/kc`, `argocd-password | …`)
//! produce clean machine-readable output. Diagnostic logs and
//! program output are on separate streams the way Unix expects.

use std::sync::Once;

use tracing_subscriber::{fmt, EnvFilter};

static INIT: Once = Once::new();

pub fn init() {
    INIT.call_once(|| {
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("warn,platform_cli=info,cli_core=info,cli_state=info,cli_providers=info")
        });

        fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_target(false)
            .compact()
            .init();
    });
}
