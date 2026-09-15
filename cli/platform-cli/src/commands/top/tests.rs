// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Every fixture here is the shape the apiserver, metrics-server and
//! the kubelet Summary API actually emit — node capacity in `Ki` and in
//! bare bytes (k3s writes allocatable `ephemeral-storage` unsuffixed),
//! metrics CPU in nanocores, pod usage split per container. Nothing in
//! this file touches a cluster.

use super::*;
use serde_json::json;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A Hetzner `cx22`-shaped k3s node: 2 vCPU, ~3.7Gi RAM, ~38Gi disk.
fn node_list() -> Value {
    json!({ "items": [ node("solo-1", false, true) ] })
}

fn node(name: &str, cordoned: bool, ready: bool) -> Value {
    json!({
        "metadata": { "name": name },
        "spec": { "unschedulable": cordoned },
        "status": {
            "capacity": {
                "cpu": "2",
                "memory": "3908620Ki",
                "ephemeral-storage": "40593708Ki",
                "pods": "110"
            },
            "allocatable": {
                // NOT equal to capacity, and deliberately in the
                // fractional-core spelling. Every AppRafter node ships
                // `kube-reserved=cpu=100m` (2.16d, `user_data.rs`), so a
                // 2-vCPU node really does report `1900m` here. While this
                // read `"2"`, reading `capacity` instead of `allocatable`
                // for CPU was a mutation the whole suite passed — the one
                // resource whose two numbers were identical was the one
                // nothing could catch.
                "cpu": "1900m",
                "memory": "3383820Ki",
                // k3s writes this one as a bare byte count.
                "ephemeral-storage": "39492531133",
                "pods": "110"
            },
            "conditions": [
                { "type": "MemoryPressure", "status": "False" },
                { "type": "Ready", "status": if ready { "True" } else { "False" } }
            ]
        }
    })
}

fn pod(
    namespace: &str,
    name: &str,
    node: Option<&str>,
    labels: Value,
    cpu: &str,
    mem: &str,
) -> Value {
    json!({
        "metadata": { "namespace": namespace, "name": name, "labels": labels },
        "spec": {
            "nodeName": node,
            "containers": [
                { "name": "main", "resources": { "requests": { "cpu": cpu, "memory": mem } } }
            ]
        },
        "status": { "phase": "Running" }
    })
}

/// One pod of each bucket, all on `solo-1`.
fn pod_list() -> Value {
    json!({ "items": [
        pod("kube-system", "cilium-abcde", Some("solo-1"),
            json!({ "k8s-app": "cilium" }), "100m", "128Mi"),
        pod("cnpg-system", "platform-postgres-1", Some("solo-1"),
            json!({ "cnpg.io/cluster": "platform-postgres", "role": "primary" }), "250m", "512Mi"),
        pod("apprafter", "shop-prod-7f9c", Some("solo-1"),
            json!({ "apprafter": "true", "apprafter.io/application": "shop",
                    "app.kubernetes.io/managed-by": "apprafter-operator" }), "50m", "64Mi"),
        pod("skunkworks", "hand-rolled", Some("solo-1"),
            json!({}), "25m", "32Mi"),
    ]})
}

fn node_metrics() -> Value {
    json!({ "kind": "NodeMetricsList", "items": [
        // metrics-server reports CPU in NANOCORES.
        { "metadata": { "name": "solo-1" },
          "usage": { "cpu": "412500000n", "memory": "1258291Ki" } }
    ]})
}

fn pod_metrics() -> Value {
    json!({ "kind": "PodMetricsList", "items": [
        { "metadata": { "namespace": "kube-system", "name": "cilium-abcde" },
          "containers": [ { "name": "cilium-agent", "usage": { "cpu": "31000000n", "memory": "102400Ki" } } ] },
        { "metadata": { "namespace": "cnpg-system", "name": "platform-postgres-1" },
          "containers": [ { "name": "postgres", "usage": { "cpu": "12000000n", "memory": "204800Ki" } } ] },
        { "metadata": { "namespace": "apprafter", "name": "shop-prod-7f9c" },
          "containers": [ { "name": "main", "usage": { "cpu": "3000000n", "memory": "20480Ki" } } ] },
        { "metadata": { "namespace": "skunkworks", "name": "hand-rolled" },
          "containers": [ { "name": "main", "usage": { "cpu": "0", "memory": "4096Ki" } } ] },
    ]})
}

fn summary() -> Value {
    json!({
        "node": {
            "nodeName": "solo-1",
            "fs": { "availableBytes": 30_000_000_000i64, "capacityBytes": 41_567_756_288i64,
                    "usedBytes": 9_000_000_000i64 }
        },
        "pods": [
            { "podRef": { "namespace": "kube-system", "name": "cilium-abcde" },
              "ephemeral-storage": { "usedBytes": 40_960 } },
            { "podRef": { "namespace": "cnpg-system", "name": "platform-postgres-1" },
              "ephemeral-storage": { "usedBytes": 1_048_576 } },
            { "podRef": { "namespace": "apprafter", "name": "shop-prod-7f9c" },
              "ephemeral-storage": { "usedBytes": 524_288 } },
            { "podRef": { "namespace": "skunkworks", "name": "hand-rolled" },
              "ephemeral-storage": { "usedBytes": 4_096 } },
        ]
    })
}

/// Every measurement present, as a healthy Tier-1 cluster serves them.
fn all_measured() -> Measurements {
    Measurements {
        nodes: parse_node_metrics(&node_metrics()),
        pods: parse_pod_metrics(&pod_metrics()),
        node_disk_used: BTreeMap::from([(
            "solo-1".to_string(),
            summary_node_disk_used(&summary()).unwrap(),
        )]),
        pod_disk_used: summary_pod_disk_used(&summary()),
        cpu_mem_absent: None,
        disk_absent: None,
    }
}

/// A cluster with no metrics-server and no reachable kubelet Summary.
fn nothing_measured() -> Measurements {
    Measurements {
        cpu_mem_absent: Some(
            "Error from server (NotFound): the server could not find the requested resource".into(),
        ),
        disk_absent: Some(
            "solo-1: Error from server (Forbidden): nodes \"solo-1\" is forbidden".into(),
        ),
        ..Measurements::default()
    }
}

fn report(measured: &Measurements) -> TopReport {
    build_report(
        &parse_nodes(&node_list()),
        &parse_pods(&pod_list()),
        measured,
    )
}

fn bucket(r: &TopReport, b: Bucket) -> &Totals {
    &r.buckets
        .iter()
        .find(|x| x.bucket == b)
        .expect("bucket present")
        .totals
}

// ---------------------------------------------------------------------------
// Node capacity
// ---------------------------------------------------------------------------

#[test]
fn a_node_yields_capacity_and_allocatable_for_all_three_resources() {
    let n = &parse_nodes(&node_list())[0];
    assert_eq!(n.name, "solo-1");
    assert_eq!(n.cpu_capacity_milli, 2000);
    assert_eq!(
        n.cpu_allocatable_milli, 1900,
        "allocatable is NOT capacity: the node reserves 100m for the kubelet"
    );
    assert_eq!(n.mem_capacity_bytes, 4_002_426_880);
    assert_eq!(n.mem_allocatable_bytes, 3_465_031_680);
    assert_eq!(n.disk_capacity_bytes, 41_567_956_992);
    // The unsuffixed k3s spelling has to parse, or the whole disk row
    // silently reads as a zero-capacity node.
    assert_eq!(n.disk_allocatable_bytes, 39_492_531_133);
    assert!(n.ready);
    assert!(!n.cordoned);
}

#[test]
fn a_node_that_reports_no_ephemeral_storage_reads_as_zero_rather_than_panicking() {
    // Some kubelets omit it entirely. A missing key is not a crash and
    // is not a guess.
    let list = json!({ "items": [ { "metadata": { "name": "n" },
        "status": { "capacity": { "cpu": "1" }, "allocatable": { "cpu": "1" } } } ]});
    let n = &parse_nodes(&list)[0];
    assert_eq!(n.disk_capacity_bytes, 0);
    assert_eq!(n.mem_allocatable_bytes, 0);
    assert_eq!(n.cpu_capacity_milli, 1000);
    assert!(!n.ready, "no Ready condition is not Ready");
}

#[test]
fn a_cordoned_notready_node_is_still_reported() {
    // It is still running its pods; it just takes no new ones. Dropping
    // it would make the cluster look smaller than it is.
    let list = json!({ "items": [ node("drained", true, false) ] });
    let n = &parse_nodes(&list)[0];
    assert!(n.cordoned);
    assert!(!n.ready);
    assert_eq!(n.cpu_capacity_milli, 2000);
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

#[test]
fn an_application_pod_is_recognised_by_any_one_of_its_three_operator_labels() {
    // `make_labels` writes all three together, so any one alone must
    // still land the pod in `applications` — otherwise dropping a single
    // key upstream reclassifies every workload in the cluster at once.
    for labels in [
        json!({ "apprafter.io/application": "shop" }),
        json!({ "apprafter": "true" }),
        json!({ "app.kubernetes.io/managed-by": "apprafter-operator" }),
    ] {
        assert_eq!(
            classify("apprafter", &labels),
            Bucket::Applications,
            "{labels}"
        );
    }
}

#[test]
fn an_application_pod_is_an_application_wherever_it_runs() {
    // The application test runs before the namespace tests, so a
    // workload scheduled into a platform or integrated namespace is
    // still charged to the application that owns it.
    for ns in ["cnpg-system", "kube-system", "somewhere-else"] {
        assert_eq!(
            classify(ns, &json!({ "apprafter.io/application": "shop" })),
            Bucket::Applications,
            "{ns}"
        );
    }
}

#[test]
fn the_integrated_data_services_are_recognised_by_namespace() {
    // Operator and operand both: the namespace exists only because the
    // integration is switched on, so its whole footprint is its cost.
    for ns in ["cnpg-system", "dragonfly-system", "nats-system"] {
        assert_eq!(classify(ns, &json!({})), Bucket::Integrated, "{ns}");
    }
}

#[test]
fn an_integrated_service_moved_to_another_namespace_is_still_integrated() {
    // `ServiceProvider.spec.config.namespace` can relocate any of the
    // three. The upstream operator's own label travels with the pod, so
    // that is what catches it.
    assert_eq!(
        classify(
            "pg-elsewhere",
            &json!({ "cnpg.io/cluster": "platform-postgres" })
        ),
        Bucket::Integrated
    );
    assert_eq!(
        classify("cache", &json!({ "app.kubernetes.io/name": "dragonfly" })),
        Bucket::Integrated
    );
    assert_eq!(
        classify("bus", &json!({ "app.kubernetes.io/name": "nats" })),
        Bucket::Integrated
    );
}

#[test]
fn the_dragonfly_operator_is_not_mistaken_for_a_dragonfly_instance_by_a_prefix() {
    // Same label key, different value. Matching loosely would make the
    // operator/operand distinction depend on a substring — this asserts
    // the match is exact. (It lands in `integrated` anyway, via its
    // namespace; what is pinned here is that the LABEL did not do it.)
    assert_eq!(
        classify(
            "elsewhere",
            &json!({ "app.kubernetes.io/name": "dragonfly-operator" })
        ),
        Bucket::Other
    );
}

#[test]
fn the_platform_namespaces_are_the_platform() {
    for ns in [
        "kube-system",
        "argocd",
        "apprafter-system",
        "cert-manager",
        "vpa",
        "backstage",
    ] {
        assert_eq!(classify(ns, &json!({})), Bucket::Platform, "{ns}");
    }
}

#[test]
fn a_pod_the_classifier_does_not_know_lands_in_other_rather_than_nowhere() {
    assert_eq!(classify("skunkworks", &json!({})), Bucket::Other);
    // `default` is nominated by two components that ship no pods, so a
    // pod there is somebody's `kubectl run` and must stay visible.
    assert_eq!(classify("default", &json!({})), Bucket::Other);
    // A pod with no labels at all is a real shape; `Value::Null` must
    // not panic.
    assert_eq!(classify("skunkworks", &Value::Null), Bucket::Other);
}

// ---------------------------------------------------------------------------
// Pod demand
// ---------------------------------------------------------------------------

#[test]
fn a_pods_request_is_the_containers_summed_the_init_containers_maxed_and_overhead_added() {
    let list = json!({ "items": [ {
        "metadata": { "namespace": "ns", "name": "p" },
        "spec": {
            "nodeName": "solo-1",
            "containers": [
                { "resources": { "requests": { "cpu": "100m", "memory": "64Mi" } } },
                { "resources": { "requests": { "cpu": "150m", "memory": "64Mi" } } }
            ],
            "initContainers": [
                { "resources": { "requests": { "cpu": "500m", "memory": "32Mi" } } },
                { "resources": { "requests": { "cpu": "300m", "memory": "16Mi" } } }
            ],
            "overhead": { "cpu": "10m", "memory": "8Mi" }
        },
        "status": { "phase": "Running" }
    } ]});
    let p = &parse_pods(&list)[0];
    // CPU: init max 500m beats the 250m sum, plus 10m overhead.
    assert_eq!(p.cpu_milli, 510);
    // Memory: the 128Mi sum beats the 32Mi init max, plus 8Mi overhead.
    assert_eq!(p.mem_bytes, 142_606_336);
    // Nothing declared ephemeral-storage, and an undeclared request is
    // zero — that is a real claim, not a missing measurement.
    assert_eq!(p.disk_bytes, 0);
}

#[test]
fn a_terminal_pod_holds_nothing_and_is_dropped() {
    // Otherwise a nightly CronJob's whole history is charged to
    // whichever bucket it ran in, forever.
    let list = json!({ "items": [
        { "metadata": { "namespace": "ns", "name": "done" },
          "spec": { "nodeName": "solo-1", "containers": [
              { "resources": { "requests": { "cpu": "1", "memory": "1Gi" } } } ] },
          "status": { "phase": "Succeeded" } },
        { "metadata": { "namespace": "ns", "name": "crashed" },
          "spec": { "nodeName": "solo-1", "containers": [
              { "resources": { "requests": { "cpu": "1", "memory": "1Gi" } } } ] },
          "status": { "phase": "Failed" } },
        { "metadata": { "namespace": "ns", "name": "live" },
          "spec": { "nodeName": "solo-1", "containers": [
              { "resources": { "requests": { "cpu": "1", "memory": "1Gi" } } } ] },
          "status": { "phase": "Running" } },
    ]});
    let pods = parse_pods(&list);
    assert_eq!(pods.len(), 1);
    assert_eq!(pods[0].name, "live");
}

#[test]
fn a_pod_the_scheduler_has_not_placed_carries_no_node() {
    let list = json!({ "items": [
        { "metadata": { "namespace": "ns", "name": "pending" },
          "spec": { "containers": [] }, "status": { "phase": "Pending" } },
        { "metadata": { "namespace": "ns", "name": "empty-nodename" },
          "spec": { "nodeName": "", "containers": [] }, "status": { "phase": "Pending" } },
    ]});
    let pods = parse_pods(&list);
    assert_eq!(pods[0].node, None);
    assert_eq!(
        pods[1].node, None,
        "an empty nodeName is not a node called \"\""
    );
}

// ---------------------------------------------------------------------------
// Measurement parsing
// ---------------------------------------------------------------------------

#[test]
fn node_metrics_arrive_in_nanocores_and_must_not_round_to_nothing() {
    // `412500000n` is 412.5 millicores. Read with a parser that only
    // knows `m`, it is `None` — and a `None` folded to zero would show a
    // busy node as idle.
    let m = parse_node_metrics(&node_metrics());
    let solo = m.get("solo-1").expect("node measured");
    assert_eq!(solo.cpu_milli, 413);
    assert_eq!(solo.mem_bytes, 1_258_291 * 1024);
}

#[test]
fn pod_metrics_are_summed_across_a_pods_containers() {
    let list = json!({ "items": [ {
        "metadata": { "namespace": "ns", "name": "sidecarred" },
        "containers": [
            { "name": "app", "usage": { "cpu": "25000000n", "memory": "100Mi" } },
            { "name": "proxy", "usage": { "cpu": "5000000n", "memory": "20Mi" } }
        ] } ]});
    let m = parse_pod_metrics(&list);
    let u = m
        .get(&("ns".to_string(), "sidecarred".to_string()))
        .unwrap();
    assert_eq!(u.cpu_milli, 30);
    assert_eq!(u.mem_bytes, 125_829_120);
}

#[test]
fn node_disk_in_use_is_capacity_minus_available() {
    // Not `usedBytes`: that is what the kubelet ATTRIBUTES, while the
    // difference is what the filesystem has actually gone. The fixture
    // makes them disagree on purpose.
    assert_eq!(summary_node_disk_used(&summary()), Some(11_567_756_288));
    assert_eq!(summary_node_disk_used(&json!({ "node": {} })), None);
}

#[test]
fn per_pod_disk_comes_from_the_summarys_ephemeral_storage() {
    let d = summary_pod_disk_used(&summary());
    assert_eq!(
        d.get(&("apprafter".to_string(), "shop-prod-7f9c".to_string())),
        Some(&524_288)
    );
    assert_eq!(d.len(), 4);
    assert!(summary_pod_disk_used(&json!({})).is_empty());
}

// ---------------------------------------------------------------------------
// The arithmetic
// ---------------------------------------------------------------------------

#[test]
fn schedulable_is_allocatable_minus_requested_and_free_is_allocatable_minus_in_use() {
    // The two are different questions and the whole command exists to
    // stop them being read as one.
    let r = report(&all_measured());
    let n = &r.nodes[0];
    assert_eq!(n.cpu.requested, 425); // 100 + 250 + 50 + 25
    assert_eq!(n.cpu.schedulable(), 1900 - 425);
    assert_eq!(n.cpu.in_use, Some(413));
    assert_eq!(n.cpu.free(), Some(1900 - 413));
    assert_eq!(n.disk.in_use, Some(11_567_756_288));
    assert_eq!(n.disk.free(), Some(39_492_531_133 - 11_567_756_288));
}

#[test]
fn an_overcommitted_node_shows_negative_headroom_rather_than_clamping_to_zero() {
    // Clamping at zero makes "exactly full" and "400% oversubscribed"
    // render identically, and the second is the one worth knowing.
    let l = Line {
        capacity: 2000,
        allocatable: 1900,
        requested: 4000,
        in_use: Some(2400),
    };
    assert_eq!(l.schedulable(), -2100);
    assert_eq!(l.free(), Some(-500));
    assert!(humanise_millicores(l.schedulable()).starts_with('-'));
}

#[test]
fn requests_are_charged_to_the_node_that_runs_the_pod() {
    let nodes = json!({ "items": [ node("a", false, true), node("b", false, true) ] });
    let pods = json!({ "items": [
        pod("apprafter", "x", Some("a"), json!({ "apprafter": "true" }), "100m", "64Mi"),
        pod("apprafter", "y", Some("b"), json!({ "apprafter": "true" }), "700m", "256Mi"),
    ]});
    let r = build_report(
        &parse_nodes(&nodes),
        &parse_pods(&pods),
        &Measurements::default(),
    );
    assert_eq!(r.nodes[0].cpu.requested, 100);
    assert_eq!(r.nodes[1].cpu.requested, 700);
    // The breakdown is cluster-wide, so it holds both.
    assert_eq!(bucket(&r, Bucket::Applications).cpu_request, 800);
}

#[test]
fn an_unscheduled_pod_is_in_the_breakdown_and_in_no_nodes_requested() {
    let pods = json!({ "items": [
        pod("apprafter", "placed", Some("solo-1"), json!({ "apprafter": "true" }), "100m", "64Mi"),
        pod("apprafter", "pending", None, json!({ "apprafter": "true" }), "900m", "512Mi"),
    ]});
    let r = build_report(
        &parse_nodes(&node_list()),
        &parse_pods(&pods),
        &Measurements::default(),
    );
    assert_eq!(r.nodes[0].cpu.requested, 100);
    assert_eq!(bucket(&r, Bucket::Applications).cpu_request, 1000);
    assert_eq!(r.unscheduled, 1);
    // And the disagreement is explained rather than left to be found.
    assert!(
        render(&r).contains("not scheduled to a node"),
        "the two tables disagree and nothing says why"
    );
}

#[test]
fn the_four_buckets_account_for_every_pod() {
    let r = report(&all_measured());
    let summed: usize = r.buckets.iter().map(|b| b.totals.pods).sum();
    assert_eq!(summed, r.total.pods);
    assert_eq!(r.total.pods, 4);
    let cpu: i64 = r.buckets.iter().map(|b| b.totals.cpu_request).sum();
    assert_eq!(cpu, r.total.cpu_request);
    assert_eq!(bucket(&r, Bucket::Platform).pods, 1);
    assert_eq!(bucket(&r, Bucket::Integrated).pods, 1);
    assert_eq!(bucket(&r, Bucket::Applications).pods, 1);
    assert_eq!(bucket(&r, Bucket::Other).pods, 1);
}

#[test]
fn measured_usage_is_attributed_to_the_bucket_that_owns_the_pod() {
    let r = report(&all_measured());
    assert_eq!(bucket(&r, Bucket::Platform).cpu_use, Some(31));
    assert_eq!(bucket(&r, Bucket::Integrated).cpu_use, Some(12));
    assert_eq!(bucket(&r, Bucket::Applications).mem_use, Some(20_971_520));
    assert_eq!(bucket(&r, Bucket::Applications).disk_use, Some(524_288));
}

#[test]
fn an_unclassified_pod_has_its_namespace_named_in_the_output() {
    // An `other` row that is normally empty is how a stale namespace
    // list announces itself — but only if the reader is told where to
    // look.
    let r = report(&all_measured());
    assert_eq!(r.other_namespaces, vec!["skunkworks".to_string()]);
    let text = render(&r);
    assert!(text.contains("`other` is not empty"), "{text}");
    assert!(text.contains("skunkworks"), "{text}");
}

#[test]
fn a_clean_cluster_says_nothing_about_other() {
    let pods = json!({ "items": [
        pod("apprafter", "x", Some("solo-1"), json!({ "apprafter": "true" }), "100m", "64Mi"),
    ]});
    let r = build_report(
        &parse_nodes(&node_list()),
        &parse_pods(&pods),
        &all_measured(),
    );
    assert!(r.other_namespaces.is_empty());
    assert!(!render(&r).contains("`other` is not empty"));
}

// ---------------------------------------------------------------------------
// Missing measurements
// ---------------------------------------------------------------------------

#[test]
fn without_metrics_in_use_and_free_are_absent_rather_than_zero() {
    // A `0m` in a column whose entire job is measurement is a lie, and
    // it is the comfortable kind nobody re-checks.
    let r = report(&nothing_measured());
    let n = &r.nodes[0];
    assert_eq!(n.cpu.in_use, None);
    assert_eq!(n.cpu.free(), None);
    assert_eq!(n.memory.in_use, None);
    assert_eq!(n.disk.in_use, None);
    assert_eq!(bucket(&r, Bucket::Platform).cpu_use, None);
    assert_eq!(bucket(&r, Bucket::Platform).mem_use, None);
    assert_eq!(bucket(&r, Bucket::Platform).disk_use, None);
    assert_eq!(r.total.cpu_use, None);
}

#[test]
fn what_was_not_measured_renders_as_a_dash_and_never_as_a_number() {
    let text = render(&report(&nothing_measured()));
    // The node table only: the footnotes below it talk ABOUT the mark,
    // and a filter that swept them up would pass on the sentence instead
    // of on the cell.
    let table = text
        .split("\nWorkload breakdown")
        .next()
        .expect("a node table");
    let mut rows = 0;
    for line in table.lines().filter(|l| {
        matches!(
            l.split_whitespace().next(),
            Some("CPU") | Some("Memory") | Some("Disk")
        )
    }) {
        rows += 1;
        assert_eq!(
            line.matches(UNMEASURED).count(),
            2,
            "IN-USE and FREE must both be unmeasured on: {line}"
        );
    }
    assert_eq!(rows, 3, "all three resource rows must be judged:\n{table}");
    // And the request-side columns are still real numbers.
    assert!(text.contains("425m"), "{text}");
}

#[test]
fn the_reason_a_measurement_is_missing_reaches_the_reader_verbatim() {
    // "no data" without a cause sends an operator to guess. The
    // cluster's own sentence is the one that names the fix.
    let text = render(&report(&nothing_measured()));
    assert!(
        text.contains("the server could not find the requested resource"),
        "{text}"
    );
    assert!(text.contains("metrics-server"), "{text}");
    assert!(text.contains("is forbidden"), "{text}");
    assert!(text.contains("stats/summary"), "{text}");
    // The half that is still trustworthy is named as such.
    assert!(text.contains("are unaffected"), "{text}");
}

#[test]
fn a_measured_cluster_prints_no_absence_notes() {
    let text = render(&report(&all_measured()));
    assert!(!text.contains("nothing was measured"), "{text}");
    assert!(!text.contains("could not be\nread"), "{text}");
    assert!(!text.contains("had no CPU / memory sample"), "{text}");
    // But it does explain why the node's disk figure exceeds the column.
    assert!(text.contains("container images"), "{text}");
}

#[test]
fn a_pod_metrics_has_no_sample_for_yet_is_counted_as_uncounted_not_as_idle() {
    // metrics-server scrapes on an interval. A pod younger than the last
    // scrape contributes 0 to its bucket, and a 0 that means "not
    // sampled" is the exact lie this command exists not to tell — so the
    // shortfall is named even though the cell cannot be blanked (the
    // other pods in the bucket were measured).
    let mut m = all_measured();
    m.pods
        .remove(&("apprafter".to_string(), "shop-prod-7f9c".to_string()));
    m.pod_disk_used
        .remove(&("apprafter".to_string(), "shop-prod-7f9c".to_string()));
    let r = build_report(&parse_nodes(&node_list()), &parse_pods(&pod_list()), &m);
    assert_eq!(r.unsampled_usage, 1);
    assert_eq!(r.unsampled_disk, 1);
    assert_eq!(bucket(&r, Bucket::Applications).cpu_use, Some(0));
    let text = render(&r);
    assert!(
        text.contains("1 pod(s) had no CPU / memory sample"),
        "{text}"
    );
    assert!(text.contains("under-count"), "{text}");
    assert!(
        text.contains("1 pod(s) had no ephemeral-storage sample"),
        "{text}"
    );
}

#[test]
fn an_unmeasurable_cluster_does_not_also_report_every_pod_as_unsampled() {
    // It would be true and useless: the columns are already `—` with a
    // reason, and a second note counting the same pods reads as a
    // separate fault.
    let r = report(&nothing_measured());
    assert_eq!(r.unsampled_usage, 0);
    assert_eq!(r.unsampled_disk, 0);
    assert!(!render(&r).contains("had no CPU / memory sample"));
}

#[test]
fn a_pending_pod_is_unscheduled_not_unsampled() {
    // A pod with no node can never have a metrics sample and is also using
    // nothing, so counting it as "not sampled yet" blames the scrape
    // interval for a pod the scrape was right to skip — and the footnote
    // then claims the USE columns under-count by an amount that is zero.
    // It is already reported on its own, as `unscheduled`.
    let mut pods = pod_list();
    pods["items"].as_array_mut().unwrap().push(pod(
        "apprafter",
        "waiting-for-room",
        None,
        json!({ "apprafter.io/application": "shop" }),
        "50m",
        "64Mi",
    ));
    let r = build_report(
        &parse_nodes(&node_list()),
        &parse_pods(&pods),
        &all_measured(),
    );
    assert_eq!(
        r.unscheduled, 1,
        "the Pending pod is reported as unscheduled"
    );
    assert_eq!(
        r.unsampled_usage, 0,
        "a Pending pod must not be counted as awaiting a scrape"
    );
    assert_eq!(r.unsampled_disk, 0);
    let text = render(&r);
    assert!(!text.contains("had no CPU / memory sample"), "{text}");
    assert!(!text.contains("under-count"), "{text}");
}

#[test]
fn a_fully_sampled_cluster_says_nothing_about_sampling() {
    let r = report(&all_measured());
    assert_eq!(r.unsampled_usage, 0);
    assert_eq!(r.unsampled_disk, 0);
}

#[test]
fn a_cluster_with_metrics_but_no_summary_blanks_only_the_disk_half() {
    // The two sources fail independently; telling the reader "no
    // metrics" while half the table is populated sends them to fix the
    // wrong thing.
    let mut m = all_measured();
    m.node_disk_used.clear();
    m.pod_disk_used.clear();
    m.disk_absent = Some("solo-1: nodes \"solo-1\" is forbidden".into());
    let r = build_report(&parse_nodes(&node_list()), &parse_pods(&pod_list()), &m);
    assert_eq!(r.nodes[0].cpu.in_use, Some(413));
    assert_eq!(r.nodes[0].disk.in_use, None);
    assert_eq!(bucket(&r, Bucket::Platform).cpu_use, Some(31));
    assert_eq!(bucket(&r, Bucket::Platform).disk_use, None);
    let text = render(&r);
    assert!(!text.contains("metrics-server"), "{text}");
    assert!(text.contains("stats/summary"), "{text}");
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

#[test]
fn every_bucket_gets_a_row_even_when_it_holds_nothing() {
    // The empty row is the point: a reader who never sees `other` cannot
    // tell an empty bucket from a bucket that was dropped.
    let pods = json!({ "items": [
        pod("kube-system", "only", Some("solo-1"), json!({}), "10m", "8Mi"),
    ]});
    let text = render(&build_report(
        &parse_nodes(&node_list()),
        &parse_pods(&pods),
        &all_measured(),
    ));
    for b in Bucket::ALL {
        assert!(
            text.contains(b.label()),
            "missing row {}: {text}",
            b.label()
        );
    }
    assert!(text.contains("TOTAL"), "{text}");
}

#[test]
fn the_six_column_headings_are_all_present_and_so_is_their_definition() {
    let text = render(&report(&all_measured()));
    for h in [
        "CAPACITY",
        "ALLOCATABLE",
        "REQUESTED",
        "SCHEDULABLE",
        "IN-USE",
        "FREE",
    ] {
        assert!(text.contains(h), "missing column {h}: {text}");
    }
    assert!(text.contains("allocatable − requested"), "{text}");
    assert!(text.contains("allocatable − in-use"), "{text}");
}

#[test]
fn a_second_node_gets_its_own_table_and_a_cluster_total() {
    let nodes = json!({ "items": [ node("a", false, true), node("b", true, true) ] });
    let pods = json!({ "items": [
        pod("apprafter", "x", Some("a"), json!({ "apprafter": "true" }), "100m", "64Mi"),
        pod("apprafter", "y", Some("b"), json!({ "apprafter": "true" }), "700m", "256Mi"),
    ]});
    let measured = Measurements {
        nodes: BTreeMap::from([
            (
                "a".into(),
                Usage {
                    cpu_milli: 300,
                    mem_bytes: 1_000_000,
                },
            ),
            (
                "b".into(),
                Usage {
                    cpu_milli: 900,
                    mem_bytes: 2_000_000,
                },
            ),
        ]),
        node_disk_used: BTreeMap::from([("a".into(), 10), ("b".into(), 20)]),
        ..Measurements::default()
    };
    let r = build_report(&parse_nodes(&nodes), &parse_pods(&pods), &measured);
    let text = render(&r);
    assert!(text.contains("Node a — Ready · schedulable"), "{text}");
    assert!(text.contains("Node b — Ready · cordoned"), "{text}");
    assert!(text.contains("Cluster total"), "{text}");
    // 2 + 2 cores of capacity, and both nodes' usage added up.
    let summed = sum_nodes(&r.nodes);
    assert_eq!(summed.cpu.capacity, 4000);
    assert_eq!(summed.cpu.in_use, Some(1200));
    assert_eq!(summed.cpu.requested, 800);
    assert_eq!(summed.disk.in_use, Some(30));
}

#[test]
fn one_node_gets_no_cluster_total_because_it_would_be_the_same_table_twice() {
    assert!(!render(&report(&all_measured())).contains("Cluster total"));
}

#[test]
fn a_cluster_total_missing_one_nodes_usage_is_absent_rather_than_short() {
    // Adding up the nodes that answered produces a number that looks
    // whole and is not — the one failure mode a total has.
    let nodes = json!({ "items": [ node("a", false, true), node("b", false, true) ] });
    let measured = Measurements {
        nodes: BTreeMap::from([(
            "a".into(),
            Usage {
                cpu_milli: 300,
                mem_bytes: 1,
            },
        )]),
        ..Measurements::default()
    };
    let r = build_report(&parse_nodes(&nodes), &[], &measured);
    assert_eq!(r.nodes[0].cpu.in_use, Some(300));
    assert_eq!(r.nodes[1].cpu.in_use, None);
    assert_eq!(sum_nodes(&r.nodes).cpu.in_use, None);
}

#[test]
fn an_empty_kubectl_response_is_an_empty_list_not_a_panic() {
    assert!(parse_nodes(&Value::Null).is_empty());
    assert!(parse_pods(&Value::Null).is_empty());
    assert!(parse_pods(&json!({ "items": [] })).is_empty());
    assert!(parse_node_metrics(&Value::Null).is_empty());
    assert!(parse_pod_metrics(&Value::Null).is_empty());
}

#[test]
fn a_kubectl_diagnostic_is_reduced_to_the_sentence_that_names_the_problem() {
    assert_eq!(
        first_line("\nError from server (NotFound): the server could not find the requested resource\nUsage:\n  kubectl get\n"),
        "Error from server (NotFound): the server could not find the requested resource"
    );
    assert_eq!(first_line("   \n\n"), "kubectl failed without a message");
}
