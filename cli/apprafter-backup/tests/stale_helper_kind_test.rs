// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Real-apiserver proof that a helper pod left from an earlier run is replaced
//! rather than failing the run that needs its name: one that has ended
//! (`Completed`, its keep-alive run out), and one still running with a spec
//! the new one cannot be applied over (an older runner's `sleep 3600`). What
//! the stub-apiserver tests in `src/kube_rs_exec.rs` cannot show is the
//! apiserver's own answer to each: the ended pod as the apply returns it, and
//! the text of the refusal to change a pod's spec in place.
//!
//! A second test shows a running helper of the SAME spec is used as it is
//! while it is new, and replaced once it has used more than
//! `RUNNING_HELPER_REUSE_MARGIN` of its keep-alive: what that needs from the
//! apiserver is the container's start in the pod the apply returns. It waits
//! out the margin, so it takes about six minutes; the two run side by side.
//!
//! Skipped by default. Opt in against a DISPOSABLE kind cluster:
//!
//! ```text
//! APPRAFTER_K8S_SMOKE=1 KUBECONFIG=<kind kubeconfig> \
//!     cargo test -p apprafter-backup --test stale_helper_kind_test -- --ignored
//! ```
//!
//! Refuses any kubeconfig whose current context is not a `kind-*` context.
//! The helper image is `docker.io/library/alpine:3.24`, pulled `IfNotPresent`
//! so it can be preloaded into the node.

use std::time::{Duration, Instant};

use apprafter_backup::kube_rs_exec::KubeRsExec;
use backup_core::KubeExec;
use k8s_openapi::api::core::v1::{Namespace, Pod};
use kube::api::{Api, DeleteParams, Patch, PatchParams};
use serde_json::{json, Value};

const NS: &str = "apprafter-stale-helper-kind";
const REUSE_NS: &str = "apprafter-stale-helper-reuse-kind";
const POD: &str = "bk-vol-stale";
const MANAGER: &str = "apprafter-stale-helper-kind";
const IMAGE: &str = "docker.io/library/alpine:3.24";

/// A helper pod as the builders shape one, in `ns`, keeping itself alive for
/// `secs`.
fn helper_in(ns: &str, secs: u64) -> Value {
    json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {
            "name": POD, "namespace": ns,
            "labels": {"apprafter.io/backup-helper": "true"}
        },
        "spec": {
            "restartPolicy": "Never",
            "containers": [{
                "name": "dump", "image": IMAGE, "imagePullPolicy": "IfNotPresent",
                "command": ["sleep", secs.to_string()]
            }]
        }
    })
}

fn helper(secs: u64) -> Value {
    helper_in(NS, secs)
}

/// A fresh namespace on the kind cluster, the runner's client and
/// [`KubeRsExec`] built as `main` builds them, and the namespace deleted on
/// drop.
struct KindNs {
    rt: tokio::runtime::Runtime,
    k: KubeRsExec,
    pods: Api<Pod>,
    namespaces: Api<Namespace>,
    ns: &'static str,
}

impl KindNs {
    fn create(ns: &'static str) -> Self {
        // Explicitly opted in, so a missing precondition is a FAILURE, not a skip.
        assert_eq!(
            std::env::var("APPRAFTER_K8S_SMOKE").as_deref(),
            Ok("1"),
            "run with APPRAFTER_K8S_SMOKE=1 (this test creates objects in the cluster)"
        );
        assert!(
            std::env::var_os("KUBECONFIG").is_some(),
            "KUBECONFIG must name the kind cluster's kubeconfig explicitly"
        );
        let kc = kube::config::Kubeconfig::read().expect("read the kubeconfig named by KUBECONFIG");
        let ctx = kc.current_context.clone().unwrap_or_default();
        assert!(
            ctx.starts_with("kind-"),
            "refusing to run against context {ctx:?}: this test only targets kind clusters"
        );

        // Built exactly as `main` builds it.
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let client = rt
            .block_on(async {
                let config = kube::Config::infer()
                    .await
                    .map_err(kube::Error::InferConfig)?;
                apprafter_backup::tls::kube_client(config)
            })
            .expect("build the kube client");
        let k = KubeRsExec::new(client.clone(), rt.handle().clone());
        let namespaces: Api<Namespace> = Api::all(client.clone());
        let pods: Api<Pod> = Api::namespaced(client.clone(), ns);

        rt.block_on(async {
            let deadline = Instant::now() + Duration::from_secs(180);
            while let Some(n) = namespaces.get_opt(ns).await.expect("get the namespace") {
                if n.status.and_then(|s| s.phase).as_deref() != Some("Terminating") {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "namespace {ns} stuck Terminating"
                );
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            namespaces
                .patch(
                    ns,
                    &PatchParams::apply(MANAGER).force(),
                    &Patch::Apply(json!({"apiVersion": "v1", "kind": "Namespace",
                                         "metadata": {"name": ns}})),
                )
                .await
                .expect("apply the namespace");
        });
        Self {
            rt,
            k,
            pods,
            namespaces,
            ns,
        }
    }

    fn uid(&self) -> String {
        self.rt
            .block_on(self.pods.get(POD))
            .expect("get the helper pod")
            .metadata
            .uid
            .expect("a pod has a uid")
    }

    fn phase(&self) -> String {
        self.rt
            .block_on(self.pods.get(POD))
            .ok()
            .and_then(|p| p.status)
            .and_then(|s| s.phase)
            .unwrap_or_default()
    }
}

impl Drop for KindNs {
    fn drop(&mut self) {
        let _ = self
            .rt
            .block_on(self.namespaces.delete(self.ns, &DeleteParams::default()));
    }
}

#[test]
#[ignore = "needs a kind cluster: APPRAFTER_K8S_SMOKE=1 KUBECONFIG=<kind kubeconfig>"]
fn a_leftover_helper_pod_is_replaced_on_a_real_apiserver() {
    let kind = KindNs::create(NS);
    let (rt, k, pods) = (&kind.rt, &kind.k, &kind.pods);
    let uid = || kind.uid();
    let phase = || kind.phase();

    // --- 1. an ENDED leftover, same spec ------------------------------------
    // The pod a run was killed before deleting: its keep-alive has run out.
    k.apply_and_wait_pod_ready(&helper(3))
        .expect("the first helper pod becomes Ready");
    let ended = uid();
    let deadline = Instant::now() + Duration::from_secs(60);
    while phase() != "Succeeded" {
        assert!(Instant::now() < deadline, "the 3 s helper never completed");
        std::thread::sleep(Duration::from_secs(1));
    }
    // The next run applies the same spec over it. Before the fix the apply
    // went through, and the run waited the full five minutes for a Ready that
    // never comes, then failed.
    let started = Instant::now();
    k.apply_and_wait_pod_ready(&helper(3))
        .expect("an ended leftover of the same spec is replaced");
    let took = started.elapsed();
    assert_ne!(uid(), ended, "a new pod, not the ended one");
    assert!(
        took < Duration::from_secs(90),
        "replacing the ended leftover took {took:?}; the Ready wait alone is 300 s"
    );
    eprintln!("ended leftover replaced in {took:?}");

    // --- 2. a RUNNING leftover whose spec cannot change in place ------------
    // An older runner's helper: `sleep 3600`, still running.
    let quick = DeleteParams {
        grace_period_seconds: Some(1),
        ..DeleteParams::default()
    };
    let _ = rt.block_on(pods.delete(POD, &quick));
    wait_gone(rt, pods);
    k.apply_and_wait_pod_ready(&helper(3600))
        .expect("the old runner's helper becomes Ready");
    let old = uid();
    let started = Instant::now();
    k.apply_and_wait_pod_ready(&helper(21600))
        .expect("a leftover with another keep-alive is replaced");
    let took = started.elapsed();
    let now = rt.block_on(pods.get(POD)).expect("get the helper pod");
    assert_ne!(now.metadata.uid.as_deref(), Some(old.as_str()));
    assert_eq!(
        now.spec.expect("spec").containers[0].command,
        Some(vec!["sleep".to_string(), "21600".to_string()]),
        "the pod now running is the new spec"
    );
    assert!(
        took < Duration::from_secs(90),
        "replacing the running leftover took {took:?}"
    );
    eprintln!("running leftover with another spec replaced in {took:?}");
}

fn wait_gone(rt: &tokio::runtime::Runtime, pods: &Api<Pod>) {
    let deadline = Instant::now() + Duration::from_secs(90);
    while rt
        .block_on(pods.get_opt(POD))
        .expect("get the helper pod")
        .is_some()
    {
        assert!(Instant::now() < deadline, "the helper pod never went");
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// A running helper of the same spec: used as it is while it is new (another
/// run's, created a moment ago), replaced once more than
/// `RUNNING_HELPER_REUSE_MARGIN` of its keep-alive has gone — the helper an
/// interrupted `backup create` leaves, which gave the next run only what was
/// left of its `sleep`.
#[test]
#[ignore = "needs a kind cluster: APPRAFTER_K8S_SMOKE=1 KUBECONFIG=<kind kubeconfig>"]
fn a_running_helper_is_reused_while_new_and_replaced_once_aged_on_a_real_apiserver() {
    let kind = KindNs::create(REUSE_NS);
    let spec = helper_in(REUSE_NS, 21600);
    kind.k
        .apply_and_wait_pod_ready(&spec)
        .expect("the helper becomes Ready");
    let first = kind.uid();

    // Moments later: the same pod.
    kind.k
        .apply_and_wait_pod_ready(&spec)
        .expect("a new helper is applied over");
    assert_eq!(kind.uid(), first, "a helper created a moment ago is reused");

    // Past the margin: a new pod, with its whole keep-alive.
    let margin = backup_core::helper_pod::RUNNING_HELPER_REUSE_MARGIN;
    std::thread::sleep(margin + Duration::from_secs(10));
    assert_eq!(kind.phase(), "Running", "the six-hour helper still runs");
    let started = Instant::now();
    kind.k
        .apply_and_wait_pod_ready(&spec)
        .expect("an aged helper is replaced");
    let took = started.elapsed();
    assert_ne!(kind.uid(), first, "a new pod, not the aged one");
    assert!(
        took < Duration::from_secs(90),
        "replacing the aged helper took {took:?}"
    );
    eprintln!("running helper aged past {margin:?} replaced in {took:?}");
}
