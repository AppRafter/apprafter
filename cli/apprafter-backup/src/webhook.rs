// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Fire-and-forget failure webhook for the in-cluster backup runner.
//!
//! A webhook failure must NEVER affect the backup run itself — every error from
//! the HTTP call is swallowed silently.  The caller does not need to handle the
//! return value.

/// POST a small JSON failure notification to `url` and discard all errors.
///
/// # Arguments
/// * `url`        — the destination URL (any scheme ureq supports).
/// * `cluster_id` — the cluster identifier to include in the payload.
/// * `phase`      — the backup phase that failed (e.g. `"backup"`, `"restore"`).
/// * `error`      — a human-readable error string.
///
/// # Guarantee
/// This function **never panics** and **never returns an error**.  Network
/// failures, invalid URLs, non-2xx responses, and serialisation errors are all
/// silently swallowed so a webhook outage cannot impact the backup run.
pub fn post_failure(url: &str, cluster_id: &str, phase: &str, error: &str) {
    let payload = serde_json::json!({
        "cluster":    cluster_id,
        "when_phase": phase,
        "error":      error,
    });
    // `ureq` returns `Err` on both network errors and non-2xx responses; we
    // discard both via `let _ =` so the caller is unaffected.
    let _ = ureq::post(url).send_json(payload);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Posting to an unreachable host must never panic (fire-and-forget
    /// guarantee).
    #[test]
    fn post_failure_swallows_errors_on_unreachable_host() {
        post_failure("http://127.0.0.1:1/nope", "c", "backup", "boom");
        // reaching here means no panic — the test passes.
    }
}
