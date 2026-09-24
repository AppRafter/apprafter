// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Real-cluster proof that a command killed by its helper pod's keep-alive is
//! explained: the exec fails with exit code 137 the moment the pod's `sleep`
//! ends, and `helper_pod::explain_keep_alive_end` reads the pod's status —
//! the container ended with exit code 0 — and says what happened and how to
//! give the command longer. The stub tests cannot show the two things that
//! matter here: the words a real apiserver uses for the killed exec, and how
//! soon the kubelet reports the container's end.
//!
//! Skipped by default. Opt in against a DISPOSABLE kind cluster:
//!
//! ```text
//! APPRAFTER_K8S_SMOKE=1 KUBECONFIG=<kind kubeconfig> \
//!     cargo test -p apprafter-backup --test keep_alive_kind_test -- --ignored
//! ```
//!
//! Refuses any kubeconfig whose current context is not a `kind-*` context.
//! The helper image is `docker.io/library/alpine:3.24`, pulled `IfNotPresent`
//! so it can be preloaded into the node. Takes about half a minute.

use std::time::{Duration, Instant};

use apprafter_backup::kube_rs_exec::KubeRsExec;
use backup_core::helper_pod::{explain_keep_alive_end, is_exit_137, keep_alive_command};
use backup_core::KubeExec;
use k8s_openapi::api::core::v1::Namespace;
use kube::api::{Api, DeleteParams, Patch, PatchParams};
use serde_json::json;

const NS: &str = "apprafter-keep-alive-kind";
const POD: &str = "ld-vol-keepalive";
const MANAGER: &str = "apprafter-keep-alive-kind";
const KEEP_ALIVE: Duration = Duration::from_secs(8);

#[test]
#[ignore = "needs a kind cluster: APPRAFTER_K8S_SMOKE=1 KUBECONFIG=<kind kubeconfig>"]
fn a_command_killed_by_its_helpers_keep_alive_is_explained() {
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
    rt.block_on(async {
        let deadline = Instant::now() + Duration::from_secs(180);
        while let Some(ns) = namespaces.get_opt(NS).await.expect("get the namespace") {
            if ns.status.and_then(|s| s.phase).as_deref() != Some("Terminating") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "namespace {NS} stuck Terminating"
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        namespaces
            .patch(
                NS,
                &PatchParams::apply(MANAGER).force(),
                &Patch::Apply(json!({"apiVersion": "v1", "kind": "Namespace",
                                     "metadata": {"name": NS}})),
            )
            .await
            .expect("apply the namespace");
    });
    struct DeleteNs<'a>(&'a tokio::runtime::Runtime, Api<Namespace>);
    impl Drop for DeleteNs<'_> {
        fn drop(&mut self) {
            let _ = self.0.block_on(self.1.delete(NS, &DeleteParams::default()));
        }
    }
    let _cleanup = DeleteNs(&rt, namespaces.clone());

    // A helper pod as the builders shape one, with a short keep-alive.
    k.apply_and_wait_pod_ready(&json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": POD, "namespace": NS,
                     "labels": {"apprafter.io/backup-helper": "true"}},
        "spec": {"restartPolicy": "Never", "containers": [{
            "name": "dump", "image": "docker.io/library/alpine:3.24",
            "imagePullPolicy": "IfNotPresent",
            "command": keep_alive_command(KEEP_ALIVE)}]}
    }))
    .expect("the helper pod becomes Ready");

    // A command that needs longer than the pod has left.
    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let err = k
        .exec_stream_to_file(POD, NS, &["sleep", "60"], &dir.path().join("out"), None)
        .expect_err("the keep-alive ends the command");
    let killed_after = started.elapsed();
    eprintln!("raw exec error after {killed_after:?}: {err}");
    assert!(
        killed_after < Duration::from_secs(30),
        "the command outlived its pod's keep-alive: {killed_after:?}"
    );
    assert!(is_exit_137(&err), "the killed exec reports 137: {err}");

    let started = Instant::now();
    let explained = explain_keep_alive_end(&k, POD, NS, err).to_string();
    eprintln!("explained after {:?}: {explained}", started.elapsed());
    assert!(
        explained.contains("keep-alive of 8s ran out"),
        "{explained}"
    );
    assert!(
        explained.contains("apprafter backup set deadline"),
        "{explained}"
    );
}
