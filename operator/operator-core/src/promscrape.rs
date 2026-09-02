// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Reading one number out of a Prometheus text endpoint. 2.22d / D8.
//!
//! # Why this exists rather than a database client
//!
//! A tenant wants to know how much data they have. The first answer was that
//! the operator has no SQL client and adding one means a connection path,
//! tenant credentials held in-process, and a new failure surface inside a
//! reconcile loop — so per-database size was filed as impractical.
//!
//! That was too quick. CloudNativePG's instance manager already runs a
//! Prometheus exporter on every instance pod, always on, and
//! `cnpg_pg_database_size_bytes{datname="…"}` is one of its DEFAULT metrics.
//! One HTTP GET through the apiserver's pod-proxy — the same shape
//! [`crate::capacity`] already uses for the kubelet Summary API — returns
//! every tenant database's size from a single scrape.
//!
//! It is also *better* than the SQL client, not merely cheaper. The exporter
//! holds its own metrics connection, so a scrape costs nothing from the
//! shared cluster's `max_connections`; a client of ours would take a slot
//! from the tenants it is measuring.
//!
//! # Scope
//!
//! Deliberately not a Prometheus client. [`parse_labelled_gauge`] is a small
//! pure reader for the one shape this needs — a gauge with one label — so it
//! tests without a cluster and without a dependency. Anything more (histogram
//! buckets, escaping edge cases, exemplars) is out of scope; if this ever
//! needs them it needs a real parser instead.

/// The value of `metric{label=value}` in a Prometheus text exposition.
///
/// Returns `None` when the metric, the label, or a parsable value is absent —
/// never a zero. "Not reported" and "reported as zero" are different facts,
/// and a size display that renders the first as the second says a tenant's
/// database is empty when it is merely unmeasured.
///
/// Matching is deliberately strict about the metric name: a line is only
/// considered when the name is followed by `{` or a space, so `cnpg_pg_x` and
/// `cnpg_pg_x_total` cannot be confused for one another.
pub fn parse_labelled_gauge(text: &str, metric: &str, label: &str, value: &str) -> Option<f64> {
    let needle = format!("{label}=\"{value}\"");
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix(metric) else {
            continue;
        };
        // The character right after the name decides whether this is our
        // metric or one whose name merely starts the same way.
        if !rest.starts_with('{') && !rest.starts_with(' ') {
            continue;
        }
        if !rest.contains(&needle) {
            continue;
        }
        // The sample value is the last whitespace-separated field. A
        // timestamp may follow it; Prometheus text puts the value first of
        // the trailing pair, so take the field after the closing brace.
        let after_labels = match rest.split_once('}') {
            Some((_, tail)) => tail,
            None => rest,
        };
        let mut fields = after_labels.split_whitespace();
        if let Some(v) = fields.next() {
            if let Ok(parsed) = v.parse::<f64>() {
                return Some(parsed);
            }
        }
    }
    None
}

/// Best-effort GET of a pod's HTTP endpoint through the apiserver pod-proxy.
///
/// `None` on any failure — an unreachable pod, a non-2xx, a body that is not
/// UTF-8. This is a decorative read: it must never fail the reconcile that
/// called it, which is the lesson the ADR 0048 anchor-403 freeze cost.
///
/// Requires `get` on `pods/proxy`, granted alongside the code that uses it.
///
/// Returns the apiserver's own error text on failure rather than swallowing
/// it. The 2.22 e2e battery spent two runs establishing only that this
/// produced nothing: the reason was logged at `debug!` and therefore invisible
/// in every deployment. A scrape that cannot say why it failed is a signal
/// that cannot be fixed.
pub async fn scrape_pod(
    client: &kube::Client,
    namespace: &str,
    pod: &str,
    port: u16,
    path: &str,
) -> Result<String, String> {
    let url = format!("/api/v1/namespaces/{namespace}/pods/{pod}:{port}/proxy{path}");
    let req = http::Request::get(&url)
        .body(Vec::new())
        .map_err(|e| format!("building the pod-proxy request: {e}"))?;
    client
        .request_text(req)
        .await
        .map_err(|e| format!("GET {url}: {e}"))
}

/// Every value a given label takes on a given metric, in the scraped body.
///
/// The companion to [`parse_labelled_gauge`]: when a lookup misses, this is
/// what turns "not found" into "found these instead", which is the difference
/// between a metric that is absent and a label value that is wrong.
pub fn label_values(body: &str, metric: &str, label: &str) -> Vec<String> {
    let needle = format!("{label}=\"");
    let mut out = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with('#') || !line.starts_with(metric) {
            continue;
        }
        let Some(open) = line.find('{') else { continue };
        let Some(close) = line.find('}') else {
            continue;
        };
        if close <= open {
            continue;
        }
        for part in line[open + 1..close].split(',') {
            let part = part.trim();
            if let Some(rest) = part.strip_prefix(&needle) {
                if let Some(v) = rest.strip_suffix('"') {
                    out.push(v.to_string());
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// TTL cache for scraped metric bodies, keyed by `namespace/pod`.
///
/// The same shape as [`crate::capacity::CapacityCache`] and for the same
/// reason: a claim reconciles on a 60s tick, and one scrape per claim per
/// tick would hammer an endpoint whose numbers move on the scale of minutes.
/// One scrape per TTL per pod serves every claim on that backend, because a
/// single CNPG scrape carries EVERY tenant database's size.
#[derive(Default)]
pub struct MetricsCache {
    entries: std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, String)>>,
    ttl: std::time::Duration,
}

impl MetricsCache {
    /// A cache with the default 300s window — the interval the size signal
    /// is worth refreshing at, matching the ACL resync loop's own tick.
    pub fn new() -> Self {
        Self {
            entries: Default::default(),
            ttl: std::time::Duration::from_secs(300),
        }
    }

    /// Scrape `pod`, serving a cached body while it is younger than the TTL.
    /// Only successes are cached, so a failing endpoint is retried every tick
    /// rather than having its failure remembered.
    pub async fn body_for_pod(
        &self,
        client: &kube::Client,
        namespace: &str,
        pod: &str,
        port: u16,
        path: &str,
    ) -> Result<String, String> {
        let key = format!("{namespace}/{pod}");
        if let Ok(map) = self.entries.lock() {
            if let Some((at, body)) = map.get(&key) {
                if at.elapsed() < self.ttl {
                    return Ok(body.clone());
                }
            }
        }
        let body = scrape_pod(client, namespace, pod, port, path).await?;
        if let Ok(mut map) = self.entries.lock() {
            map.insert(key, (std::time::Instant::now(), body.clone()));
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
# HELP cnpg_pg_database_size_bytes Disk space used by the database
# TYPE cnpg_pg_database_size_bytes gauge
cnpg_pg_database_size_bytes{datname="postgres"} 8.529419e+06
cnpg_pg_database_size_bytes{datname="claim-demo-web-pg"} 1.234e+07
cnpg_pg_database_size_bytes_total{datname="claim-demo-web-pg"} 999
cnpg_pg_database_xid_age{datname="claim-demo-web-pg"} 42
"#;

    #[test]
    fn it_reads_the_gauge_for_one_label_value() {
        let v = parse_labelled_gauge(
            SAMPLE,
            "cnpg_pg_database_size_bytes",
            "datname",
            "claim-demo-web-pg",
        );
        assert_eq!(v, Some(1.234e7));
    }

    #[test]
    fn a_metric_whose_name_merely_starts_the_same_is_not_matched() {
        // `cnpg_pg_database_size_bytes_total` shares a prefix with the metric
        // we want and carries the same label. A naive `starts_with` would
        // return 999 here — a wrong number that looks perfectly plausible,
        // which is the worst failure a size display can have.
        let v = parse_labelled_gauge(
            SAMPLE,
            "cnpg_pg_database_size_bytes",
            "datname",
            "claim-demo-web-pg",
        );
        assert_ne!(v, Some(999.0));
    }

    #[test]
    fn a_different_label_value_is_a_different_database() {
        assert_eq!(
            parse_labelled_gauge(SAMPLE, "cnpg_pg_database_size_bytes", "datname", "postgres"),
            Some(8.529419e6)
        );
    }

    #[test]
    fn an_absent_database_is_none_and_never_zero() {
        // "Not reported" and "reported as zero" are different facts. A size
        // display that renders the first as the second tells a tenant their
        // database is empty when it is merely unmeasured.
        assert_eq!(
            parse_labelled_gauge(
                SAMPLE,
                "cnpg_pg_database_size_bytes",
                "datname",
                "no-such-db"
            ),
            None
        );
        assert_eq!(
            parse_labelled_gauge(SAMPLE, "no_such_metric", "datname", "postgres"),
            None
        );
    }

    #[test]
    fn comments_are_not_samples() {
        // The HELP line contains the metric name and would otherwise parse
        // as a sample with a garbage value.
        let only_help = "# HELP cnpg_pg_database_size_bytes Disk space used\n";
        assert_eq!(
            parse_labelled_gauge(only_help, "cnpg_pg_database_size_bytes", "datname", "x"),
            None
        );
    }

    #[test]
    fn a_trailing_timestamp_does_not_become_the_value() {
        let with_ts = "m{d=\"x\"} 1234 1699999999000\n";
        assert_eq!(parse_labelled_gauge(with_ts, "m", "d", "x"), Some(1234.0));
    }
}
