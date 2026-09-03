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
    let url = pod_proxy_path(namespace, pod, port, path);
    let req = http::Request::get(&url)
        .body(Vec::new())
        .map_err(|e| format!("building the pod-proxy request: {e}"))?;
    client
        .request_text(req)
        .await
        .map_err(|e| format!("GET {url}: {e}"))
}

/// The apiserver path that proxies to `pod`'s HTTP endpoint.
///
/// Split out of [`scrape_pod`] so the one part of that function that is not
/// I/O can be pinned: the pod-proxy subresource is addressed as
/// `pods/{name}:{port}`, and every plausible slip — proxying to the namespace
/// instead of the pod, dropping the port, joining `path` without its leading
/// slash — yields a 404 that reads as "the endpoint is down" rather than "we
/// asked the wrong question".
fn pod_proxy_path(namespace: &str, pod: &str, port: u16, path: &str) -> String {
    format!("/api/v1/namespaces/{namespace}/pods/{pod}:{port}/proxy{path}")
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
        self.body_for_key(cache_key(namespace, pod), || {
            scrape_pod(client, namespace, pod, port, path)
        })
        .await
    }

    /// Serve `key` from the cache while it is fresh, else `fetch` it and
    /// remember the result — but only if it succeeded.
    ///
    /// The fetch is a parameter rather than a hard-wired [`scrape_pod`] so the
    /// cache's two load-bearing policies are reachable without an apiserver: a
    /// fresh entry must suppress the fetch ENTIRELY (one scrape per TTL is
    /// what keeps a per-claim 60s reconcile from hammering the exporter), and
    /// a failure must not be cached (remembering one would blind every claim
    /// on that backend for the rest of the window). Neither is visible from
    /// the outside — a cache that quietly stopped caching, or one that
    /// remembered an error, both just look slow.
    async fn body_for_key<F, Fut>(&self, key: String, fetch: F) -> Result<String, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<String, String>>,
    {
        if let Some(body) = self.cached_fresh(&key) {
            return Ok(body);
        }
        let body = fetch().await?;
        self.store(key, body.clone());
        Ok(body)
    }

    /// The cached body for `key` iff it is younger than the TTL.
    ///
    /// Separated from the fetch so the cache's whole decision — which key a
    /// pod maps to, and when a sample stops being servable — is reachable
    /// without a cluster; the scrape either side of it is not.
    fn cached_fresh(&self, key: &str) -> Option<String> {
        let map = self.entries.lock().ok()?;
        let (at, body) = map.get(key)?;
        if at.elapsed() < self.ttl {
            Some(body.clone())
        } else {
            None
        }
    }

    /// Insert/replace the cached body for `key`, stamped now.
    fn store(&self, key: String, body: String) {
        if let Ok(mut map) = self.entries.lock() {
            map.insert(key, (std::time::Instant::now(), body));
        }
    }

    /// Scrape ignoring the cache, and refresh it with the result.
    ///
    /// For the one case the TTL genuinely gets wrong: a body cached BEFORE an
    /// object existed does not mention it, and "the metric is absent" then
    /// looks exactly like "the metric is not published". A newly created
    /// database would otherwise report no size for up to the full TTL — five
    /// minutes of a freshly provisioned claim showing nothing, which is
    /// indistinguishable from the feature being broken. It cost three e2e
    /// rounds to tell those apart.
    ///
    /// Only worth calling once, after a miss: if the fresh body does not carry
    /// the value either, the value is genuinely not there.
    pub async fn body_for_pod_uncached(
        &self,
        client: &kube::Client,
        namespace: &str,
        pod: &str,
        port: u16,
        path: &str,
    ) -> Result<String, String> {
        self.refresh_key(cache_key(namespace, pod), || {
            scrape_pod(client, namespace, pod, port, path)
        })
        .await
    }

    /// Fetch `key` unconditionally and replace whatever was cached for it.
    ///
    /// The counterpart to [`Self::body_for_key`], with the fetch injected for
    /// the same reason: what distinguishes this path is that it does NOT
    /// consult the cache, and a version that quietly started to would
    /// reintroduce the five-minute blind spot on a freshly created database
    /// that the doc comment above describes.
    async fn refresh_key<F, Fut>(&self, key: String, fetch: F) -> Result<String, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<String, String>>,
    {
        let body = fetch().await?;
        self.store(key, body.clone());
        Ok(body)
    }
}

/// Cache key for one pod's scraped body.
///
/// Namespace-qualified because pod names are only unique within a namespace,
/// and the thing being cached is a body carrying EVERY tenant database's
/// size: a key collision would serve one namespace's backend metrics as
/// another's, which is a wrong number rather than a missing one.
fn cache_key(namespace: &str, pod: &str) -> String {
    format!("{namespace}/{pod}")
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

    #[test]
    fn a_sample_whose_value_cannot_be_read_does_not_stop_the_scan() {
        // A truncated line (the scrape raced the exporter mid-write) or a
        // non-numeric one must be skipped, not treated as the answer for that
        // label: the reader keeps looking and finds the real sample. Giving up
        // at the first unreadable line reports "no size" for a database whose
        // size is right there on the next line.
        let body = "m{d=\"x\"}\nm{d=\"x\"} not-a-number\nm{d=\"x\"} 42\n";
        assert_eq!(parse_labelled_gauge(body, "m", "d", "x"), Some(42.0));
    }

    // -----------------------------------------------------------------
    // label_values — the "found these instead" miss diagnostic
    // -----------------------------------------------------------------

    /// A scrape carrying several metrics, only one of which is the one being
    /// looked up. `claim-zzz` belongs to a DIFFERENT metric and must never be
    /// reported as a database this metric knows about.
    const MIXED: &str = r#"
# HELP cnpg_pg_database_size_bytes Disk space used by the database
# TYPE cnpg_pg_database_size_bytes gauge
cnpg_pg_database_size_bytes{datname="postgres"} 8.5e+06
cnpg_pg_database_size_bytes{datname="claim-a"} 1.0e+06
cnpg_pg_database_size_bytes{datname="claim-a"} 2.0e+06
cnpg_pg_replication_slots{datname="claim-zzz"} 1
cnpg_collector_up 1
"#;

    #[test]
    fn label_values_lists_only_the_asked_for_metrics_own_label_values() {
        // Sorted + deduplicated, scoped to the requested metric. The caller
        // (resourceclaim-provisioner's pg size read) renders this straight
        // into a failure message as "exists for datname [...] but not for X",
        // so a value borrowed from a neighbouring metric sends whoever reads
        // it looking for a database that this metric never mentioned.
        assert_eq!(
            label_values(MIXED, "cnpg_pg_database_size_bytes", "datname"),
            vec!["claim-a".to_string(), "postgres".to_string()]
        );
    }

    #[test]
    fn label_values_is_empty_when_the_metric_is_not_published_at_all() {
        // Emptiness is a decision, not a detail: the caller branches on it to
        // choose between "this cluster does not publish the metric — go
        // enable it" and "it publishes it, just not for your database". A
        // list that is never empty would send every operator to enable a
        // metric that was already on, and one that is always empty would hide
        // the databases that ARE there.
        let body = "cnpg_pg_replication_slots{datname=\"claim-zzz\"} 1\ncnpg_collector_up 1\n";
        assert!(label_values(body, "cnpg_pg_database_size_bytes", "datname").is_empty());
        assert!(!label_values(MIXED, "cnpg_pg_database_size_bytes", "datname").is_empty());
    }

    #[test]
    fn label_values_reads_one_label_out_of_a_multi_label_sample() {
        // Real CNPG samples carry several labels; the value wanted is the one
        // label's, not the whole `{...}` block.
        let body = "m{cluster=\"pg-shared\",datname=\"claim-a\",role=\"primary\"} 3\n";
        assert_eq!(
            label_values(body, "m", "datname"),
            vec!["claim-a".to_string()]
        );
    }

    // -----------------------------------------------------------------
    // pod-proxy addressing + the scrape cache
    // -----------------------------------------------------------------

    #[test]
    fn the_scrape_url_addresses_the_pod_proxy_subresource() {
        // `pods/{name}:{port}/proxy{path}` — the apiserver answers a wrong
        // shape here with a 404 that reads exactly like a dead endpoint.
        assert_eq!(
            pod_proxy_path("cnpg-system", "pg-shared-1", 9187, "/metrics"),
            "/api/v1/namespaces/cnpg-system/pods/pg-shared-1:9187/proxy/metrics"
        );
    }

    #[test]
    fn a_stored_body_is_served_back_while_it_is_fresh() {
        let cache = MetricsCache::new();
        cache.store(cache_key("cnpg-system", "pg-1"), "body".to_string());
        assert_eq!(
            cache.cached_fresh(&cache_key("cnpg-system", "pg-1")),
            Some("body".to_string())
        );
        // A pod nothing was stored for is a miss, not someone else's body.
        assert_eq!(cache.cached_fresh(&cache_key("cnpg-system", "pg-2")), None);
    }

    #[test]
    fn the_cache_key_is_namespace_qualified() {
        // Pod names are unique only within a namespace, and the cached body
        // carries EVERY tenant database's size on that backend. A collision
        // would serve one namespace's sizes under another's name — a wrong
        // number, which is worse than the missing one a cache miss gives.
        let cache = MetricsCache::new();
        cache.store(cache_key("tenant-a", "pg-1"), "a".to_string());
        cache.store(cache_key("tenant-b", "pg-1"), "b".to_string());
        assert_eq!(
            cache.cached_fresh(&cache_key("tenant-a", "pg-1")),
            Some("a".to_string())
        );
        assert_eq!(
            cache.cached_fresh(&cache_key("tenant-b", "pg-1")),
            Some("b".to_string())
        );
    }

    /// A fetch that records how often it ran, so "did not scrape" is an
    /// assertion rather than an assumption.
    struct CountingFetch {
        calls: std::cell::Cell<u32>,
        result: Result<String, String>,
    }

    impl CountingFetch {
        fn ok(body: &str) -> Self {
            Self {
                calls: std::cell::Cell::new(0),
                result: Ok(body.to_string()),
            }
        }
        fn err(msg: &str) -> Self {
            Self {
                calls: std::cell::Cell::new(0),
                result: Err(msg.to_string()),
            }
        }
        async fn run(&self) -> Result<String, String> {
            self.calls.set(self.calls.get() + 1);
            self.result.clone()
        }
    }

    #[tokio::test]
    async fn a_fresh_cached_body_suppresses_the_scrape_entirely() {
        // The reason the cache exists: every claim on a backend reconciles on
        // its own 60s tick and they all read the SAME body. A cache that
        // served the right answer but still scraped would keep the numbers
        // correct and quietly multiply load on the exporter by the tenant
        // count — invisible until the backend is the thing that falls over.
        let cache = MetricsCache::new();
        cache.store(cache_key("cnpg-system", "pg-1"), "cached".to_string());
        let fetch = CountingFetch::ok("scraped");
        let got = cache
            .body_for_key(cache_key("cnpg-system", "pg-1"), || fetch.run())
            .await;
        assert_eq!(got, Ok("cached".to_string()));
        assert_eq!(fetch.calls.get(), 0);
    }

    #[tokio::test]
    async fn a_failed_scrape_is_not_remembered() {
        // Caching a failure would blind every claim on that backend for the
        // whole 300s window over one unlucky moment, and the claims would
        // report "no size" rather than "the scrape failed".
        let cache = MetricsCache::new();
        let failing = CountingFetch::err("GET …: connection refused");
        let err = cache
            .body_for_key(cache_key("cnpg-system", "pg-1"), || failing.run())
            .await;
        assert!(err.is_err(), "{err:?}");
        assert_eq!(cache.cached_fresh(&cache_key("cnpg-system", "pg-1")), None);

        // …and the very next tick gets a real attempt, not the remembered one.
        let ok = CountingFetch::ok("scraped");
        let got = cache
            .body_for_key(cache_key("cnpg-system", "pg-1"), || ok.run())
            .await;
        assert_eq!(got, Ok("scraped".to_string()));
        assert_eq!(ok.calls.get(), 1);
    }

    #[tokio::test]
    async fn a_miss_scrapes_once_and_the_body_is_then_cached() {
        let cache = MetricsCache::new();
        let fetch = CountingFetch::ok("scraped");
        let got = cache
            .body_for_key(cache_key("cnpg-system", "pg-1"), || fetch.run())
            .await;
        assert_eq!(got, Ok("scraped".to_string()));
        assert_eq!(fetch.calls.get(), 1);
        assert_eq!(
            cache.cached_fresh(&cache_key("cnpg-system", "pg-1")),
            Some("scraped".to_string())
        );
    }

    #[tokio::test]
    async fn an_uncached_read_scrapes_past_a_fresh_body_and_replaces_it() {
        // The case the TTL gets wrong: a body captured before a database
        // existed cannot mention it, and "absent from the metric" then looks
        // exactly like "the metric is not published". Consulting the cache
        // here would restore the five-minute window in which a freshly
        // provisioned claim reports nothing — three e2e rounds went into
        // telling those two apart.
        let cache = MetricsCache::new();
        cache.store(cache_key("cnpg-system", "pg-1"), "old".to_string());
        let fetch = CountingFetch::ok("new");
        let got = cache
            .refresh_key(cache_key("cnpg-system", "pg-1"), || fetch.run())
            .await;
        assert_eq!(got, Ok("new".to_string()));
        assert_eq!(fetch.calls.get(), 1);
        // …and the fresher body replaces the stale one for everybody else.
        assert_eq!(
            cache.cached_fresh(&cache_key("cnpg-system", "pg-1")),
            Some("new".to_string())
        );
    }

    #[test]
    fn a_body_older_than_the_ttl_is_not_served() {
        // `Instant` cannot be constructed in the past portably, so the TTL
        // side of the same comparison is driven to zero instead: every stored
        // sample is then already older than the window.
        let cache = MetricsCache {
            entries: Default::default(),
            ttl: std::time::Duration::ZERO,
        };
        cache.store(cache_key("cnpg-system", "pg-1"), "stale".to_string());
        assert_eq!(cache.cached_fresh(&cache_key("cnpg-system", "pg-1")), None);
    }
}
