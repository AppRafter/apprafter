// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter top` — what the node has, what is spoken for, what is
//! actually being used, and who is using it. Plan §2.27e.
//!
//! Six numbers per resource, because "how full is my node" has six
//! different answers and an operator asking the question needs to know
//! which one they got:
//!
//! | Column        | Meaning                                              |
//! |---------------|------------------------------------------------------|
//! | `CAPACITY`    | what the machine physically has                      |
//! | `ALLOCATABLE` | what the kubelet will hand to pods (capacity minus the reservations `apprafter node prep` sets, minus eviction thresholds) |
//! | `REQUESTED`   | what the scheduled pods have *claimed*               |
//! | `SCHEDULABLE` | `allocatable − requested` — what a new pod could still ask for |
//! | `IN-USE`      | what is *measured* to be in use right now            |
//! | `FREE`        | `allocatable − in-use` — what is genuinely unused    |
//!
//! `REQUESTED`/`SCHEDULABLE` and `IN-USE`/`FREE` are the two halves that
//! get conflated. A node can be 95% requested and 10% used (the usual
//! Kubernetes over-declaration) or 20% requested and 95% used (nothing
//! declares limits and one pod is eating the box). Those are opposite
//! problems with opposite fixes, and a single "used" number cannot tell
//! them apart.
//!
//! # Measured means measured
//!
//! `IN-USE` and `FREE` come from `metrics.k8s.io` (CPU, memory) and the
//! kubelet Summary API (disk). Both can be absent — a cluster without
//! metrics-server serves neither, and a node that has just booted has no
//! samples yet. When they are, those cells render as `—` and the reason
//! is printed underneath. **They never render as `0`**: a zero in a
//! column whose entire job is measurement is a lie, and it is the
//! comfortable kind that nobody re-checks.
//!
//! # Pure core
//!
//! Everything except [`measure`] and [`run`] is a pure function over
//! `serde_json::Value`, so the parsing, the classification, the
//! arithmetic and the rendering are all tested against fixtures with no
//! cluster anywhere near them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use cli_core::quantity::{humanise_bytes, humanise_millicores, parse_bytes, parse_millicores};
use cli_core::{CliError, Result};
use serde_json::Value;
use tabled::{settings::Style, Table, Tabled};

use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, kubectl_get_json, kubectl_get_json_cluster_wide,
};

// ---------------------------------------------------------------------------
// Classification vocabulary
// ---------------------------------------------------------------------------

/// Namespaces the platform itself runs in.
///
/// Derived from `platform-stack/cue/component_*.cue` (each component's
/// `namespace:` field) plus the three namespaces Kubernetes ships. The
/// platform-stack CUE is the source of truth and lives in a different
/// language in a different directory, so this cannot be a compile-time
/// projection of it; the [`Bucket::Other`] escape hatch below is what
/// makes a drift visible rather than silent.
///
/// `default` is deliberately NOT here. Two components nominate it
/// (`gateway-api-crds`, `network-policies`) but neither puts a pod in it
/// — both ship cluster-scoped objects and Argo CD demands a namespace
/// field regardless. A pod in `default` is somebody's `kubectl run`, and
/// it should surface as [`Bucket::Other`] rather than be absorbed into
/// the platform's row.
const PLATFORM_NAMESPACES: &[&str] = &[
    "apprafter-system",
    "argocd",
    "backstage",
    "cert-manager",
    "kube-node-lease",
    "kube-public",
    "kube-system",
    "vpa",
];

/// Namespaces the integrated data services run in.
///
/// Each holds both the upstream operator and the instances it manages
/// (`cnpg-system` carries the CNPG controller *and* `platform-postgres`;
/// `dragonfly-system` the operator *and* every `platform-redis-*`). The
/// split is not drawn between controller and operand on purpose: the
/// namespace exists only because that integration is switched on, so its
/// whole footprint is the cost of `needs.pg` / `needs.redis` /
/// `needs.jetstream` and is what an operator deciding whether to keep
/// using them needs to see.
///
/// `needs.disk` and `SharedVolume` appear in neither list because they
/// stand up no pod at all — they are a PVC mounted into the application's
/// own pod, so their CPU and memory are the application's.
const INTEGRATED_NAMESPACES: &[&str] = &["cnpg-system", "dragonfly-system", "nats-system"];

/// The per-application grouping label the operator's `make_labels`
/// stamps on every workload pod template
/// (`operator/operator-rendering/src/lib.rs`).
const APP_GROUPING_LABEL: &str = "apprafter.io/application";

/// The bare marker from the same label set. Spelled exactly as
/// `app.rs`'s `BUNDLE_POD_SELECTOR` (`"apprafter=true"`), which is the
/// selector `app status` already uses to find a bundle's pods.
const APP_MARKER_LABEL: &str = "apprafter";
const APP_MARKER_VALUE: &str = "true";

/// The well-known `managed-by` key, and the value the operator writes.
const MANAGED_BY_LABEL: &str = "app.kubernetes.io/managed-by";
const OPERATOR_MANAGED_BY: &str = "apprafter-operator";

/// CloudNativePG labels every instance pod with this
/// (`operator/operator-rendering/src/egress.rs` — the deprecated
/// `postgresql.cnpg.io/cluster` spelling matches nothing and was a
/// shipped bug). Presence alone is the test: it catches a Cluster
/// provisioned into a namespace overridden through
/// `ServiceProvider.spec.config.namespace`.
const CNPG_CLUSTER_LABEL: &str = "cnpg.io/cluster";

/// The well-known `name` key, and the exact values the two other
/// integrated services' pods carry.
///
/// Exact values, not a prefix: the Dragonfly *operator* carries
/// `dragonfly-operator` under the same key, and matching loosely would
/// make the distinction between operator and operand depend on a
/// substring.
const WORKLOAD_NAME_LABEL: &str = "app.kubernetes.io/name";
const INTEGRATED_WORKLOAD_NAMES: &[&str] = &["dragonfly", "nats"];

/// Which of the three parts of the platform a pod belongs to.
///
/// [`Bucket::Other`] is the fourth on purpose. Every pod lands in
/// exactly one bucket and the four sum to the cluster, so a pod the
/// classifier does not recognise cannot quietly go missing from the
/// totals — it shows up in a row that is normally empty, which is how a
/// stale namespace list announces itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bucket {
    /// Everything the platform runs to be a platform.
    Platform,
    /// The data services `needs.*` provisions, operators included.
    Integrated,
    /// User workloads rendered from an `Application`.
    Applications,
    /// Matched nothing. Normally empty; never hidden.
    Other,
}

impl Bucket {
    /// Every bucket, in the order the breakdown prints them:
    /// platform first (it is the floor an operator is trying to see
    /// past), applications last before the catch-all.
    pub const ALL: [Bucket; 4] = [
        Bucket::Platform,
        Bucket::Integrated,
        Bucket::Applications,
        Bucket::Other,
    ];

    /// The row label.
    pub fn label(self) -> &'static str {
        match self {
            Bucket::Platform => "platform",
            Bucket::Integrated => "integrated services",
            Bucket::Applications => "applications",
            Bucket::Other => "other",
        }
    }
}

/// Which bucket a pod in `namespace` carrying `labels` belongs to.
///
/// Order is load-bearing and runs narrowest-first. The application test
/// comes before the integrated one because an application pod scheduled
/// into an integrated namespace must still read as an application;
/// labels come before namespaces within the integrated test because
/// `ServiceProvider.spec.config.namespace` can move an instance out of
/// the default namespace, and the upstream operator's label travels with
/// the pod when it does.
///
/// `labels` is the raw `metadata.labels` object — `Value::Null` for a
/// pod that carries none, which is a real shape and must not panic.
pub fn classify(namespace: &str, labels: &Value) -> Bucket {
    let label = |k: &str| labels.get(k).and_then(Value::as_str);

    // Any one of the three is enough. `make_labels` writes all of them
    // together, so requiring the conjunction would only mean that a
    // future change to any single key silently reclassifies every
    // application pod as `other`.
    if label(APP_GROUPING_LABEL).is_some()
        || label(APP_MARKER_LABEL) == Some(APP_MARKER_VALUE)
        || label(MANAGED_BY_LABEL) == Some(OPERATOR_MANAGED_BY)
    {
        return Bucket::Applications;
    }

    if label(CNPG_CLUSTER_LABEL).is_some()
        || label(WORKLOAD_NAME_LABEL).is_some_and(|n| INTEGRATED_WORKLOAD_NAMES.contains(&n))
        || INTEGRATED_NAMESPACES.contains(&namespace)
    {
        return Bucket::Integrated;
    }

    if PLATFORM_NAMESPACES.contains(&namespace) {
        return Bucket::Platform;
    }

    Bucket::Other
}

// ---------------------------------------------------------------------------
// Node capacity
// ---------------------------------------------------------------------------

/// One node's declared capacity and allocatable, parsed.
///
/// Each figure is `Option` because a node that does not report one is a
/// node whose number we do not know — the arithmetic downstream treats a
/// missing capacity as zero for display but never claims to have
/// measured it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeCapacity {
    pub name: String,
    /// `status.conditions[type=Ready].status == "True"`.
    pub ready: bool,
    /// `spec.unschedulable == true` — a cordoned node still runs its
    /// pods, so its rows are real; it just takes no new ones.
    pub cordoned: bool,
    pub cpu_capacity_milli: i64,
    pub cpu_allocatable_milli: i64,
    pub mem_capacity_bytes: i64,
    pub mem_allocatable_bytes: i64,
    pub disk_capacity_bytes: i64,
    pub disk_allocatable_bytes: i64,
}

/// Parse a `NodeList` (`kubectl get nodes -o json`).
pub fn parse_nodes(list: &Value) -> Vec<NodeCapacity> {
    items(list)
        .iter()
        .map(|n| {
            let q = |section: &str, key: &str, parse: fn(&str) -> Option<i64>| {
                n.pointer(&format!("/status/{section}"))
                    .and_then(|s| s.get(key))
                    .and_then(Value::as_str)
                    .and_then(parse)
                    .unwrap_or(0)
            };
            NodeCapacity {
                name: n
                    .pointer("/metadata/name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                ready: node_ready(n),
                cordoned: n
                    .pointer("/spec/unschedulable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                cpu_capacity_milli: q("capacity", "cpu", parse_millicores),
                cpu_allocatable_milli: q("allocatable", "cpu", parse_millicores),
                mem_capacity_bytes: q("capacity", "memory", parse_bytes),
                mem_allocatable_bytes: q("allocatable", "memory", parse_bytes),
                disk_capacity_bytes: q("capacity", "ephemeral-storage", parse_bytes),
                disk_allocatable_bytes: q("allocatable", "ephemeral-storage", parse_bytes),
            }
        })
        .collect()
}

fn node_ready(node: &Value) -> bool {
    node.pointer("/status/conditions")
        .and_then(Value::as_array)
        .map(|cs| {
            cs.iter().any(|c| {
                c.get("type").and_then(Value::as_str) == Some("Ready")
                    && c.get("status").and_then(Value::as_str) == Some("True")
            })
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Pod demand
// ---------------------------------------------------------------------------

/// What one pod has claimed, and which bucket it claimed it for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodDemand {
    pub namespace: String,
    pub name: String,
    /// `spec.nodeName` — `None` for a pod the scheduler has not placed
    /// yet. Such a pod holds no node's resources but has still declared
    /// its requests, which is why the breakdown and the node tables can
    /// legitimately disagree.
    pub node: Option<String>,
    pub bucket: Bucket,
    pub cpu_milli: i64,
    pub mem_bytes: i64,
    pub disk_bytes: i64,
}

/// Parse a `PodList` (`kubectl get pods -A -o json`), dropping the pods
/// that hold nothing.
///
/// `Succeeded` and `Failed` are terminal: the kubelet has released their
/// resources and the scheduler does not count them. Counting them would
/// charge a nightly CronJob's whole history to whichever bucket it ran in.
pub fn parse_pods(list: &Value) -> Vec<PodDemand> {
    items(list)
        .iter()
        .filter(|p| !is_terminal(p))
        .map(|p| {
            let spec = p.get("spec").cloned().unwrap_or(Value::Null);
            let namespace = p
                .pointer("/metadata/namespace")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let labels = p
                .pointer("/metadata/labels")
                .cloned()
                .unwrap_or(Value::Null);
            PodDemand {
                bucket: classify(&namespace, &labels),
                namespace,
                name: p
                    .pointer("/metadata/name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                node: spec
                    .get("nodeName")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
                cpu_milli: effective_request(&spec, "cpu", parse_millicores),
                mem_bytes: effective_request(&spec, "memory", parse_bytes),
                disk_bytes: effective_request(&spec, "ephemeral-storage", parse_bytes),
            }
        })
        .collect()
}

fn is_terminal(pod: &Value) -> bool {
    matches!(
        pod.pointer("/status/phase").and_then(Value::as_str),
        Some("Succeeded") | Some("Failed")
    )
}

/// The pod's effective request for one resource, by the scheduler's own
/// formula: `max(sum(containers), max(initContainers)) + overhead`.
///
/// Init containers run one at a time and before the rest, so the pod's
/// floor is the largest single init container — not their sum. `overhead`
/// is the runtime's own cost (Kata charges real memory per sandbox on
/// tiers 3 and 4) and is added on top of both.
///
/// Not modelled: a sidecar init container (`restartPolicy: Always`),
/// which runs alongside the main set and so adds rather than maxes.
/// Nothing the platform ships uses one.
fn effective_request(spec: &Value, key: &str, parse: fn(&str) -> Option<i64>) -> i64 {
    let list = |k: &str| {
        spec.get(k)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    };
    let one = |c: &Value| {
        c.pointer("/resources/requests")
            .and_then(|r| r.get(key))
            .and_then(Value::as_str)
            .and_then(parse)
            .unwrap_or(0)
    };
    let regular: i64 = list("containers").iter().map(one).sum();
    let init: i64 = list("initContainers").iter().map(one).max().unwrap_or(0);
    let overhead = spec
        .get("overhead")
        .and_then(|o| o.get(key))
        .and_then(Value::as_str)
        .and_then(parse)
        .unwrap_or(0);
    regular.max(init) + overhead
}

// ---------------------------------------------------------------------------
// Measurement
// ---------------------------------------------------------------------------

/// A measured CPU + memory pair.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub cpu_milli: i64,
    pub mem_bytes: i64,
}

/// Everything that was actually measured, plus — when it was not — the
/// reason, in the words the cluster used.
///
/// The two absence fields are separate because the two sources fail
/// independently: a cluster can serve `metrics.k8s.io` perfectly and
/// refuse `nodes/proxy`, and telling the reader "no metrics" when half
/// the table is populated would send them to fix the wrong thing.
#[derive(Debug, Clone, Default)]
pub struct Measurements {
    /// Per-node CPU + memory, keyed by node name.
    pub nodes: BTreeMap<String, Usage>,
    /// Per-pod CPU + memory, keyed by `(namespace, name)`.
    pub pods: BTreeMap<(String, String), Usage>,
    /// Per-node root-filesystem bytes in use, keyed by node name.
    pub node_disk_used: BTreeMap<String, i64>,
    /// Per-pod ephemeral-storage bytes in use, keyed by `(namespace, name)`.
    pub pod_disk_used: BTreeMap<(String, String), i64>,
    /// Why CPU and memory were not measured. `None` ⇒ they were.
    pub cpu_mem_absent: Option<String>,
    /// Why disk was not measured. `None` ⇒ it was.
    pub disk_absent: Option<String>,
}

/// Parse a `NodeMetricsList` from `metrics.k8s.io/v1beta1`.
///
/// metrics-server reports CPU in **nanocores** (`"137452089n"`), which is
/// why `cli_core::quantity::parse_millicores` has to understand the `n`
/// suffix — see the note there.
pub fn parse_node_metrics(list: &Value) -> BTreeMap<String, Usage> {
    items(list)
        .iter()
        .filter_map(|m| {
            let name = m.pointer("/metadata/name").and_then(Value::as_str)?;
            Some((name.to_string(), usage_of(m.get("usage")?)))
        })
        .collect()
}

/// Parse a `PodMetricsList`, summing each pod's containers.
pub fn parse_pod_metrics(list: &Value) -> BTreeMap<(String, String), Usage> {
    items(list)
        .iter()
        .filter_map(|m| {
            let ns = m.pointer("/metadata/namespace").and_then(Value::as_str)?;
            let name = m.pointer("/metadata/name").and_then(Value::as_str)?;
            let mut total = Usage::default();
            for c in m
                .get("containers")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[])
            {
                if let Some(u) = c.get("usage") {
                    let u = usage_of(u);
                    total.cpu_milli += u.cpu_milli;
                    total.mem_bytes += u.mem_bytes;
                }
            }
            Some(((ns.to_string(), name.to_string()), total))
        })
        .collect()
}

fn usage_of(usage: &Value) -> Usage {
    Usage {
        cpu_milli: usage
            .get("cpu")
            .and_then(Value::as_str)
            .and_then(parse_millicores)
            .unwrap_or(0),
        mem_bytes: usage
            .get("memory")
            .and_then(Value::as_str)
            .and_then(parse_bytes)
            .unwrap_or(0),
    }
}

/// Bytes in use on the node's root filesystem, from a kubelet Summary
/// document (`/api/v1/nodes/<n>/proxy/stats/summary`).
///
/// `capacity − available`, not `usedBytes`: `usedBytes` on `node.fs` is
/// what the kubelet attributes, while the difference is what the
/// filesystem actually has gone. The operator reads the same two fields
/// for `NodeDiskPressure` (`operator-core/src/capacity.rs`).
pub fn summary_node_disk_used(summary: &Value) -> Option<i64> {
    let avail = summary.pointer("/node/fs/availableBytes")?.as_i64()?;
    let cap = summary.pointer("/node/fs/capacityBytes")?.as_i64()?;
    Some(cap - avail)
}

/// Per-pod ephemeral-storage bytes in use from the same document.
///
/// This is the pod's writable container layer, its logs and its
/// `emptyDir`s. It is NOT the contents of a PersistentVolume and NOT the
/// container images, which is why these never sum to the node figure
/// above — the output says so rather than leaving a reader to discover
/// it by subtraction.
pub fn summary_pod_disk_used(summary: &Value) -> BTreeMap<(String, String), i64> {
    summary
        .get("pods")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|p| {
            let ns = p.pointer("/podRef/namespace").and_then(Value::as_str)?;
            let name = p.pointer("/podRef/name").and_then(Value::as_str)?;
            let used = p
                .pointer("/ephemeral-storage/usedBytes")
                .and_then(Value::as_i64)?;
            Some(((ns.to_string(), name.to_string()), used))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

/// One resource's six numbers for one node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line {
    pub capacity: i64,
    pub allocatable: i64,
    pub requested: i64,
    /// `None` when the measurement was not available. Never `0` for that
    /// case — see the module docs.
    pub in_use: Option<i64>,
}

impl Line {
    /// `allocatable − requested`: what a new pod could still ask for.
    /// Goes negative on an over-committed node, and is shown negative.
    pub fn schedulable(&self) -> i64 {
        self.allocatable - self.requested
    }

    /// `allocatable − in_use`: what is genuinely unused. `None` — not
    /// zero — when nothing was measured.
    pub fn free(&self) -> Option<i64> {
        self.in_use.map(|u| self.allocatable - u)
    }
}

/// One node's three resource lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeReport {
    pub name: String,
    pub ready: bool,
    pub cordoned: bool,
    pub cpu: Line,
    pub memory: Line,
    pub disk: Line,
}

/// What some set of pods asked for and what it is measured to be using.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Totals {
    pub pods: usize,
    pub cpu_request: i64,
    pub cpu_use: Option<i64>,
    pub mem_request: i64,
    pub mem_use: Option<i64>,
    pub disk_request: i64,
    pub disk_use: Option<i64>,
}

/// One bucket's share of the cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BucketReport {
    pub bucket: Bucket,
    pub totals: Totals,
}

/// Everything `apprafter top` prints.
#[derive(Debug, Clone)]
pub struct TopReport {
    pub nodes: Vec<NodeReport>,
    /// One entry per [`Bucket::ALL`], always all four.
    pub buckets: Vec<BucketReport>,
    /// Every pod, counted again from scratch rather than by adding the
    /// four rows up. A bucket list that lost a variant would otherwise
    /// produce a total that agreed with its own omission.
    pub total: Totals,
    /// Pods the scheduler has not placed. Their requests are in the
    /// breakdown and in no node's `REQUESTED`.
    pub unscheduled: usize,
    /// Pods `metrics.k8s.io` was serving but had no sample for — a pod
    /// younger than the first scrape. They contribute 0 to their
    /// bucket's `CPU USE` / `MEM USE`, which under-counts it, so the
    /// count is carried out to the reader rather than absorbed. Always
    /// 0 when [`TopReport::cpu_mem_absent`] is set: that case is already
    /// a `—` and a reason.
    pub unsampled_usage: usize,
    /// The same for the kubelet's per-pod `ephemeral-storage`, which it
    /// does not report for every volume plugin.
    pub unsampled_disk: usize,
    /// The namespaces behind a non-empty [`Bucket::Other`], so a drifted
    /// classifier can be diagnosed from the output.
    pub other_namespaces: Vec<String>,
    pub cpu_mem_absent: Option<String>,
    pub disk_absent: Option<String>,
}

/// Fold nodes, pods and measurements into the printable report.
pub fn build_report(
    nodes: &[NodeCapacity],
    pods: &[PodDemand],
    measured: &Measurements,
) -> TopReport {
    let node_reports = nodes
        .iter()
        .map(|n| {
            let on_node = |f: fn(&PodDemand) -> i64| -> i64 {
                pods.iter()
                    .filter(|p| p.node.as_deref() == Some(n.name.as_str()))
                    .map(f)
                    .sum()
            };
            let usage = measured.nodes.get(&n.name).copied();
            NodeReport {
                name: n.name.clone(),
                ready: n.ready,
                cordoned: n.cordoned,
                cpu: Line {
                    capacity: n.cpu_capacity_milli,
                    allocatable: n.cpu_allocatable_milli,
                    requested: on_node(|p| p.cpu_milli),
                    in_use: usage.map(|u| u.cpu_milli),
                },
                memory: Line {
                    capacity: n.mem_capacity_bytes,
                    allocatable: n.mem_allocatable_bytes,
                    requested: on_node(|p| p.mem_bytes),
                    in_use: usage.map(|u| u.mem_bytes),
                },
                disk: Line {
                    capacity: n.disk_capacity_bytes,
                    allocatable: n.disk_allocatable_bytes,
                    requested: on_node(|p| p.disk_bytes),
                    in_use: measured.node_disk_used.get(&n.name).copied(),
                },
            }
        })
        .collect();

    let buckets: Vec<BucketReport> = Bucket::ALL
        .iter()
        .map(|&b| BucketReport {
            bucket: b,
            totals: totals_of(pods.iter().filter(|p| p.bucket == b), measured),
        })
        .collect();
    let total = totals_of(pods.iter(), measured);

    let other_namespaces: BTreeSet<String> = pods
        .iter()
        .filter(|p| p.bucket == Bucket::Other)
        .map(|p| p.namespace.clone())
        .collect();

    let missing = |present: bool, held: &dyn Fn(&(String, String)) -> bool| {
        if !present {
            return 0;
        }
        pods.iter()
            .filter(|p| !held(&(p.namespace.clone(), p.name.clone())))
            .count()
    };

    TopReport {
        nodes: node_reports,
        buckets,
        total,
        unscheduled: pods.iter().filter(|p| p.node.is_none()).count(),
        unsampled_usage: missing(measured.cpu_mem_absent.is_none(), &|k| {
            measured.pods.contains_key(k)
        }),
        unsampled_disk: missing(measured.disk_absent.is_none(), &|k| {
            measured.pod_disk_used.contains_key(k)
        }),
        other_namespaces: other_namespaces.into_iter().collect(),
        cpu_mem_absent: measured.cpu_mem_absent.clone(),
        disk_absent: measured.disk_absent.clone(),
    }
}

fn totals_of<'a>(pods: impl Iterator<Item = &'a PodDemand>, measured: &Measurements) -> Totals {
    let mut out = Totals {
        pods: 0,
        cpu_request: 0,
        cpu_use: (measured.cpu_mem_absent.is_none()).then_some(0),
        mem_request: 0,
        mem_use: (measured.cpu_mem_absent.is_none()).then_some(0),
        disk_request: 0,
        disk_use: (measured.disk_absent.is_none()).then_some(0),
    };
    for p in pods {
        out.pods += 1;
        out.cpu_request += p.cpu_milli;
        out.mem_request += p.mem_bytes;
        out.disk_request += p.disk_bytes;
        let key = (p.namespace.clone(), p.name.clone());
        if let Some(u) = measured.pods.get(&key) {
            if let Some(c) = out.cpu_use.as_mut() {
                *c += u.cpu_milli;
            }
            if let Some(m) = out.mem_use.as_mut() {
                *m += u.mem_bytes;
            }
        }
        if let Some(d) = measured.pod_disk_used.get(&key) {
            if let Some(t) = out.disk_use.as_mut() {
                *t += d;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The em-dash that stands for "not measured". One constant, because a
/// reader learning what it means on one table must find the same mark on
/// the other.
const UNMEASURED: &str = "—";

#[derive(Tabled)]
struct NodeTableRow {
    #[tabled(rename = "RESOURCE")]
    resource: String,
    #[tabled(rename = "CAPACITY")]
    capacity: String,
    #[tabled(rename = "ALLOCATABLE")]
    allocatable: String,
    #[tabled(rename = "REQUESTED")]
    requested: String,
    #[tabled(rename = "SCHEDULABLE")]
    schedulable: String,
    #[tabled(rename = "IN-USE")]
    in_use: String,
    #[tabled(rename = "FREE")]
    free: String,
}

#[derive(Tabled)]
struct BucketTableRow {
    #[tabled(rename = "BUCKET")]
    bucket: String,
    #[tabled(rename = "PODS")]
    pods: String,
    #[tabled(rename = "CPU REQ")]
    cpu_request: String,
    #[tabled(rename = "CPU USE")]
    cpu_use: String,
    #[tabled(rename = "MEM REQ")]
    mem_request: String,
    #[tabled(rename = "MEM USE")]
    mem_use: String,
    #[tabled(rename = "DISK REQ")]
    disk_request: String,
    #[tabled(rename = "DISK USE")]
    disk_use: String,
}

fn node_rows(n: &NodeReport) -> Vec<NodeTableRow> {
    let row = |name: &str, l: &Line, f: fn(i64) -> String| NodeTableRow {
        resource: name.to_string(),
        capacity: f(l.capacity),
        allocatable: f(l.allocatable),
        requested: f(l.requested),
        schedulable: f(l.schedulable()),
        in_use: l.in_use.map(f).unwrap_or_else(|| UNMEASURED.to_string()),
        free: l.free().map(f).unwrap_or_else(|| UNMEASURED.to_string()),
    };
    vec![
        row("CPU", &n.cpu, humanise_millicores),
        row("Memory", &n.memory, humanise_bytes),
        row("Disk", &n.disk, humanise_bytes),
    ]
}

fn bucket_row(label: &str, b: &Totals) -> BucketTableRow {
    let m =
        |v: Option<i64>, f: fn(i64) -> String| v.map(f).unwrap_or_else(|| UNMEASURED.to_string());
    BucketTableRow {
        bucket: label.to_string(),
        pods: b.pods.to_string(),
        cpu_request: humanise_millicores(b.cpu_request),
        cpu_use: m(b.cpu_use, humanise_millicores),
        mem_request: humanise_bytes(b.mem_request),
        mem_use: m(b.mem_use, humanise_bytes),
        disk_request: humanise_bytes(b.disk_request),
        disk_use: m(b.disk_use, humanise_bytes),
    }
}

/// Render the whole screen. Pure — the tests read exactly what an
/// operator reads.
pub fn render(report: &TopReport) -> String {
    let mut out = String::new();

    for n in &report.nodes {
        let state = format!(
            "{} · {}",
            if n.ready { "Ready" } else { "NotReady" },
            if n.cordoned {
                "cordoned"
            } else {
                "schedulable"
            }
        );
        out.push_str(&format!("\nNode {} — {state}\n", n.name));
        out.push_str(&format!(
            "{}\n",
            Table::new(node_rows(n)).with(Style::blank())
        ));
    }

    if report.nodes.len() > 1 {
        out.push_str("\nCluster total\n");
        let summed = sum_nodes(&report.nodes);
        out.push_str(&format!(
            "{}\n",
            Table::new(node_rows(&summed)).with(Style::blank())
        ));
    }

    out.push_str(&format!(
        "\nWorkload breakdown — {} pods, cluster-wide\n",
        report.total.pods
    ));
    let mut rows: Vec<BucketTableRow> = report
        .buckets
        .iter()
        .map(|b| bucket_row(b.bucket.label(), &b.totals))
        .collect();
    rows.push(bucket_row("TOTAL", &report.total));
    out.push_str(&format!("{}\n", Table::new(rows).with(Style::blank())));

    for note in notes(report) {
        out.push_str(&format!("\n{note}\n"));
    }
    out
}

/// A synthetic node report that is every node added together. Its
/// `in_use` is `None` unless EVERY node was measured: a total that
/// silently omits one node's usage reads as a smaller cluster.
fn sum_nodes(nodes: &[NodeReport]) -> NodeReport {
    let fold = |f: fn(&NodeReport) -> &Line| Line {
        capacity: nodes.iter().map(|n| f(n).capacity).sum(),
        allocatable: nodes.iter().map(|n| f(n).allocatable).sum(),
        requested: nodes.iter().map(|n| f(n).requested).sum(),
        in_use: nodes
            .iter()
            .map(|n| f(n).in_use)
            .try_fold(0i64, |acc, v| v.map(|v| acc + v)),
    };
    NodeReport {
        name: "cluster".to_string(),
        ready: nodes.iter().all(|n| n.ready),
        cordoned: nodes.iter().all(|n| n.cordoned),
        cpu: fold(|n| &n.cpu),
        memory: fold(|n| &n.memory),
        disk: fold(|n| &n.disk),
    }
}

/// The footnotes, in the order a reader needs them: first what the
/// columns mean, then why any of them are blank, then the two places
/// where the two tables legitimately disagree.
fn notes(report: &TopReport) -> Vec<String> {
    let mut out = vec![
        "SCHEDULABLE is allocatable − requested: what a new pod could still ask for.\n\
         FREE is allocatable − in-use: what is unused right now. A node can be fully \
         requested\nand barely used, or barely requested and full; these two columns are \
         what tells them apart."
            .to_string(),
    ];

    if let Some(why) = &report.cpu_mem_absent {
        out.push(format!(
            "CPU and memory IN-USE / FREE read `{UNMEASURED}` because nothing was measured. \
             `metrics.k8s.io`\nanswered:\n  {why}\nInstall or repair metrics-server \
             (`kubectl top` reads the same API). CAPACITY, ALLOCATABLE,\nREQUESTED and \
             SCHEDULABLE come from the node and pod specs and are unaffected."
        ));
    } else if report.unsampled_usage > 0 {
        out.push(format!(
            "{} pod(s) had no CPU / memory sample yet — metrics-server scrapes on an \
             interval, so a\npod younger than the last scrape has none. The CPU USE and MEM \
             USE columns under-count\nthose pods by however much they are using; the node's \
             own IN-USE does not.",
            report.unsampled_usage
        ));
    }

    match &report.disk_absent {
        Some(why) => out.push(format!(
            "Disk IN-USE / FREE and the DISK USE column read `{UNMEASURED}` because the \
             kubelet Summary API\n(`/api/v1/nodes/<node>/proxy/stats/summary`) could not be \
             read. It answered:\n  {why}\nDisk CAPACITY, ALLOCATABLE and REQUESTED are the \
             node's `ephemeral-storage` and are unaffected."
        )),
        None => {
            out.push(
                "Disk is `ephemeral-storage` — the node's root filesystem. The node's IN-USE \
                 is the whole\nfilesystem (OS, container images, logs and volume data \
                 included), so it is larger than the\nDISK USE column, which counts only each \
                 pod's writable layer, logs and emptyDir volumes."
                    .to_string(),
            );
            if report.unsampled_disk > 0 {
                out.push(format!(
                    "{} pod(s) had no ephemeral-storage sample — the kubelet does not report \
                     one for every\nvolume plugin. The DISK USE column under-counts those pods.",
                    report.unsampled_disk
                ));
            }
        }
    }

    if report.unscheduled > 0 {
        out.push(format!(
            "{} pod(s) are not scheduled to a node. Their requests are in the breakdown and \
             in no\nnode's REQUESTED, so the two tables will not add up until they are placed.",
            report.unscheduled
        ));
    }

    if !report.other_namespaces.is_empty() {
        out.push(format!(
            "`other` is not empty: {} pod(s) matched no bucket, in namespace(s) {}.\nThat is \
             either a workload the platform did not create, or this build's classifier is \
             behind\nthe cluster — please report the latter.",
            report
                .buckets
                .iter()
                .find(|b| b.bucket == Bucket::Other)
                .map(|b| b.totals.pods)
                .unwrap_or(0),
            report.other_namespaces.join(", ")
        ));
    }

    out
}

// ---------------------------------------------------------------------------
// Fetch layer (the only part that touches a cluster)
// ---------------------------------------------------------------------------

fn items(list: &Value) -> Vec<Value> {
    list.get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// `kubectl get --raw <path>`, with the cluster's own words on failure.
///
/// Its own spawn rather than a `k8s_helpers` getter because the two APIs
/// this reaches are not `kubectl get`-able resources on every cluster,
/// and because the STDERR is the product here: "the server could not
/// find the requested resource" is what tells a reader metrics-server is
/// missing, and a classified error type would replace it with our guess.
fn kubectl_raw(path: &str, kubeconfig: &Path) -> std::result::Result<Value, String> {
    let out = Command::new("kubectl")
        .args(["get", "--raw", path])
        .env("KUBECONFIG", kubeconfig)
        .output()
        .map_err(|e| format!("could not run kubectl: {e}"))?;
    if !out.status.success() {
        return Err(first_line(&String::from_utf8_lossy(&out.stderr)));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("{path}: response was not JSON: {e}"))
}

/// The first non-empty line of a kubectl diagnostic, which is the
/// sentence that names the problem; the rest is usage text.
fn first_line(stderr: &str) -> String {
    stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("kubectl failed without a message")
        .to_string()
}

/// Read every measurement that is available, recording why for every one
/// that is not. Never fails: an unmeasurable cluster still gets its
/// capacity and requests table.
fn measure(nodes: &[NodeCapacity], kubeconfig: &Path) -> Measurements {
    let mut m = Measurements::default();

    match (
        kubectl_raw("/apis/metrics.k8s.io/v1beta1/nodes", kubeconfig),
        kubectl_raw("/apis/metrics.k8s.io/v1beta1/pods", kubeconfig),
    ) {
        (Ok(n), Ok(p)) => {
            m.nodes = parse_node_metrics(&n);
            m.pods = parse_pod_metrics(&p);
        }
        (Err(why), _) | (_, Err(why)) => m.cpu_mem_absent = Some(why),
    }

    // One Summary fetch per node, and one failure is enough to blank the
    // column: a disk total assembled from some of the nodes is a number
    // that looks whole and is not.
    let mut disk_failure = None;
    for n in nodes {
        match kubectl_raw(
            &format!("/api/v1/nodes/{}/proxy/stats/summary", n.name),
            kubeconfig,
        ) {
            Ok(summary) => {
                if let Some(used) = summary_node_disk_used(&summary) {
                    m.node_disk_used.insert(n.name.clone(), used);
                }
                m.pod_disk_used.extend(summary_pod_disk_used(&summary));
            }
            Err(why) => {
                disk_failure.get_or_insert(format!("{}: {why}", n.name));
            }
        }
    }
    if let Some(why) = disk_failure {
        m.node_disk_used.clear();
        m.pod_disk_used.clear();
        m.disk_absent = Some(why);
    }

    m
}

/// `apprafter top`. Read-only throughout.
pub fn run() -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;

    let nodes_json = kubectl_get_json("nodes", None, None, kc.path())?.ok_or_else(|| {
        CliError::Other("the cluster returned no node list — is the apiserver reachable?".into())
    })?;
    let nodes = parse_nodes(&nodes_json);
    if nodes.is_empty() {
        return Err(CliError::Other(
            "the cluster reports no nodes, so there is no capacity to show".into(),
        ));
    }

    let pods_json = kubectl_get_json_cluster_wide("pods", None, kc.path())?.unwrap_or(Value::Null);
    let pods = parse_pods(&pods_json);

    let measured = measure(&nodes, kc.path());
    print!("{}", render(&build_report(&nodes, &pods, &measured)));
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
