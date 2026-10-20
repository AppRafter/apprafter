// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A one-node tier-1 cluster as `apprafter top` reads it: the node, its pods, metrics-server's
//! samples and the kubelet's Summary, in the shapes they emit (capacity in `Ki` and in bare
//! bytes, CPU samples in nanocores) — the shapes `top`'s own tests use.

use serde_json::{json, Value};

use super::list;

/// The node's name.
pub const NODE: &str = "solo-1";

/// A Hetzner `cx22`-shaped k3s node: 2 vCPU, ~3.7 GiB, ~38 GiB of disk, 100m of CPU reserved
/// (allocatable `1900m`), allocatable disk in bare bytes as k3s writes it.
pub fn node(name: &str, cordoned: bool, ready: bool) -> Value {
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
                "cpu": "1900m",
                "memory": "3383820Ki",
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

/// `kubectl get nodes -o json` of the one-node cluster.
pub fn solo_nodes() -> Value {
    list(vec![node(NODE, false, true)])
}

/// A pod requesting `cpu` and `mem` in one container, on `node` (`None`: not scheduled yet,
/// `Pending`).
pub fn pod(
    namespace: &str,
    name: &str,
    node: Option<&str>,
    labels: Value,
    cpu: &str,
    mem: &str,
) -> Value {
    let phase = if node.is_some() { "Running" } else { "Pending" };
    json!({
        "metadata": { "namespace": namespace, "name": name, "labels": labels },
        "spec": {
            "nodeName": node,
            "containers": [
                { "name": "main", "resources": { "requests": { "cpu": cpu, "memory": mem } } }
            ]
        },
        "status": { "phase": phase }
    })
}

/// The labels the operator's `make_labels` puts on an application's pods.
fn app_labels(app: &str) -> Value {
    json!({
        "apprafter": "true",
        "apprafter.io/application": app,
        "app.kubernetes.io/managed-by": "apprafter-operator"
    })
}

/// One pod of each bucket but `other`, all on [`NODE`]: Cilium (platform), the shared
/// Postgres (integrated services), an application.
pub fn solo_pods() -> Vec<Value> {
    vec![
        pod(
            "kube-system",
            "cilium-abcde",
            Some(NODE),
            json!({ "k8s-app": "cilium" }),
            "100m",
            "128Mi",
        ),
        pod(
            "cnpg-system",
            "platform-postgres-1",
            Some(NODE),
            json!({ "cnpg.io/cluster": "platform-postgres", "role": "primary" }),
            "250m",
            "512Mi",
        ),
        pod(
            "shop",
            "shop-prod-7f9c",
            Some(NODE),
            app_labels("shop"),
            "50m",
            "64Mi",
        ),
    ]
}

/// An application pod no node has taken yet.
pub fn pending_pod() -> Value {
    pod(
        "shop",
        "shop-prod-9d1e",
        None,
        app_labels("shop"),
        "50m",
        "64Mi",
    )
}

/// A pod no bucket claims: a `kubectl run` in a namespace of its own.
pub fn other_pod() -> Value {
    pod(
        "skunkworks",
        "hand-rolled",
        Some(NODE),
        json!({}),
        "25m",
        "32Mi",
    )
}

/// metrics-server's sample of [`NODE`]: CPU in nanocores.
pub fn node_metrics() -> Value {
    json!({
        "kind": "NodeMetricsList",
        "apiVersion": "metrics.k8s.io/v1beta1",
        "items": [
            { "metadata": { "name": NODE }, "usage": { "cpu": "412500000n", "memory": "1258291Ki" } }
        ]
    })
}

/// Whether a node has taken `pod` (critic round, finding 6: short lines rustfmt keeps).
fn is_scheduled(pod: &Value) -> bool {
    let node = pod["spec"]["nodeName"].as_str();
    node.is_some_and(|n| !n.is_empty())
}

/// The pods of `pods` a node has taken.
fn scheduled(pods: &[Value]) -> impl Iterator<Item = &Value> {
    pods.iter().filter(|p| is_scheduled(p))
}

/// metrics-server's sample of each scheduled pod of `pods`: 3m of CPU, 20 MiB.
pub fn pod_metrics(pods: &[Value]) -> Value {
    let items: Vec<Value> = scheduled(pods)
        .map(|p| {
            json!({
                "metadata": { "namespace": p["metadata"]["namespace"], "name": p["metadata"]["name"] },
                "containers": [ { "name": "main", "usage": { "cpu": "3000000n", "memory": "20480Ki" } } ]
            })
        })
        .collect();
    json!({ "kind": "PodMetricsList", "apiVersion": "metrics.k8s.io/v1beta1", "items": items })
}

/// The kubelet's Summary of [`NODE`] running `pods`: its root filesystem, and 512 KiB of each
/// scheduled pod's ephemeral storage.
pub fn summary(pods: &[Value]) -> Value {
    let entries: Vec<Value> = scheduled(pods)
        .map(|p| {
            json!({
                "podRef": { "namespace": p["metadata"]["namespace"], "name": p["metadata"]["name"] },
                "ephemeral-storage": { "usedBytes": 524288 }
            })
        })
        .collect();
    json!({
        "node": {
            "nodeName": NODE,
            "fs": {
                "availableBytes": 30000000000_i64,
                "capacityBytes": 41567756288_i64,
                "usedBytes": 9000000000_i64
            }
        },
        "pods": entries
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pod_metrics_and_summaries_cover_each_scheduled_pod() {
        let mut pods = solo_pods();
        pods.push(pending_pod());
        let sampled = pod_metrics(&pods);
        let summarised = summary(&pods);
        let samples = sampled["items"].as_array().expect("items");
        assert_eq!(samples.len(), 3);
        assert_eq!(summarised["pods"].as_array().map(Vec::len), Some(3));
        assert!(samples
            .iter()
            .all(|s| s["metadata"]["name"] != "shop-prod-9d1e"));
    }
}
