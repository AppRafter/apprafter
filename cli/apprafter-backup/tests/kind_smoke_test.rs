// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Real-apiserver smoke test for the backup runner's kube-rs surface: the
//! [`KubeRsExec`] methods a scheduled backup uses, plus the status-ConfigMap
//! write, over real TLS and the real `pods/exec` WebSocket. What the
//! stub-apiserver unit tests in `src/kube_rs_exec.rs` cannot reach — the
//! socket, rustls' handshake, the WebSocket upgrade and a real apiserver's
//! `Status` bodies — is what this covers.
//!
//! Skipped by default. Opt in against a DISPOSABLE kind cluster:
//!
//! ```text
//! APPRAFTER_K8S_SMOKE=1 KUBECONFIG=<kind kubeconfig> \
//!     cargo test -p apprafter-backup --test kind_smoke_test -- --ignored
//! ```
//!
//! It refuses any kubeconfig whose current context is not a `kind-*` context:
//! it creates namespaces, a Secret, a Pod and a ConfigMap, and must never reach
//! a real cluster by accident. The helper pod's image defaults to
//! `docker.io/library/alpine:3.24` (the runner image's own base; override with
//! `APPRAFTER_SMOKE_IMAGE`) and is pulled `IfNotPresent`, so it can be
//! preloaded into the node.
//!
//! NOT covered: `exec_stream_from_file` (stdin INTO a pod). Against Kubernetes
//! 1.36 it delivers only a prefix of the input while the remote command still
//! reports `Success` — observed identically with kube-rs 0.95 and 4.2, while
//! `kubectl exec -i` delivers the same input in full. The scheduled runner
//! never streams stdin (restore runs through the CLI's `kubectl` path), so no
//! shipped path reaches it; asserting it here would only pin the defect.
//!
//! Run inside its own process, which is what makes the first assertion — no
//! crypto provider installed before the runner's own code — meaningful.

use std::time::Duration;

use apprafter_backup::kube_rs_exec::KubeRsExec;
use apprafter_backup::orchestrate::RunOutcome;
use apprafter_backup::status::write_status;
use backup_core::KubeExec;
use k8s_openapi::api::core::v1::{ConfigMap, Namespace, Secret};
use kube::api::{Api, DeleteParams, Patch, PatchParams};
use serde_json::json;

const NS: &str = "apprafter-backup-smoke";
const STATUS_NS: &str = "apprafter-system";
const STATUS_CM: &str = "apprafter-backup-status";
const POD: &str = "bk-smoke";
const MANAGER: &str = "apprafter-backup-smoke";

/// `metav1.Time` wire form: whole seconds, UTC, `Z`.
fn is_metav1_time(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 20
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 | 16 => *c == b':',
            19 => *c == b'Z',
            _ => c.is_ascii_digit(),
        })
}

#[test]
#[ignore = "real cluster — set APPRAFTER_K8S_SMOKE=1 + KUBECONFIG (a kind cluster) to run"]
fn smoke_kube_rs_exec_against_a_kind_cluster() {
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
        "refusing to run against context {ctx:?}: this smoke test only targets kind clusters"
    );

    assert!(
        rustls::crypto::CryptoProvider::get_default().is_none(),
        "a crypto provider was installed before the runner's code ran"
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

    // --- setup: namespaces + a Secret, by server-side apply -----------------
    rt.block_on(async {
        let ns_api: Api<Namespace> = Api::all(client.clone());
        // The previous run's cleanup leaves NS Terminating for a while, and a
        // Terminating namespace accepts the apply below but rejects the pod.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
        while let Some(ns) = ns_api.get_opt(NS).await.expect("get the smoke namespace") {
            if ns.status.and_then(|s| s.phase).as_deref() != Some("Terminating") {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "namespace {NS} is stuck Terminating"
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        for ns in [NS, STATUS_NS] {
            ns_api
                .patch(
                    ns,
                    &PatchParams::apply(MANAGER).force(),
                    &Patch::Apply(json!({"apiVersion": "v1", "kind": "Namespace",
                                         "metadata": {"name": ns}})),
                )
                .await
                .unwrap_or_else(|e| panic!("apply namespace {ns}: {e}"));
        }
        let secrets: Api<Secret> = Api::namespaced(client.clone(), NS);
        secrets
            .patch(
                "pg-0-conn",
                &PatchParams::apply(MANAGER).force(),
                &Patch::Apply(json!({"apiVersion": "v1", "kind": "Secret",
                                     "metadata": {"name": "pg-0-conn", "namespace": NS},
                                     "stringData": {"pass": "s3cr3t/pw"}})),
            )
            .await
            .expect("apply the smoke Secret");
        // A stale status CM from an earlier run would make the merge
        // assertions below vacuous.
        let cms: Api<ConfigMap> = Api::namespaced(client.clone(), STATUS_NS);
        let _ = cms.delete(STATUS_CM, &DeleteParams::default()).await;
    });

    // --- get_json: cluster-scoped GET through core discovery -----------------
    let ks = k
        .get_json(&["get", "namespaces", "kube-system", "-o", "json"])
        .expect("get namespace kube-system")
        .expect("kube-system exists");
    assert!(
        ks["metadata"]["uid"]
            .as_str()
            .is_some_and(|u| !u.is_empty()),
        "kube-system must carry a uid (the cluster's machine key): {ks}"
    );
    assert_eq!(ks["kind"], json!("Namespace"));
    // The timestamp a REAL apiserver sent, re-serialized through the typed
    // ObjectMeta, must be byte-identical to what the apiserver sent.
    let raw: serde_json::Value = rt
        .block_on(
            client.request::<serde_json::Value>(
                http::Request::get("/api/v1/namespaces/kube-system")
                    .body(Vec::new())
                    .unwrap(),
            ),
        )
        .expect("raw GET kube-system");
    let sent = raw["metadata"]["creationTimestamp"]
        .as_str()
        .expect("the apiserver sent a creationTimestamp");
    assert!(is_metav1_time(sent), "unexpected wire form {sent:?}");
    assert_eq!(ks["metadata"]["creationTimestamp"], json!(sent));
    assert_eq!(
        ks["metadata"]["managedFields"][0]["time"],
        raw["metadata"]["managedFields"][0]["time"]
    );

    // --- get_json: a real 404 is Ok(None); a list is the kubectl envelope ----
    assert!(k
        .get_json(&["get", "secrets", "does-not-exist", "-n", NS, "-o", "json"])
        .expect("a 404 from a real apiserver must not be an error")
        .is_none());
    let listed = k
        .get_json(&["get", "secrets", "-n", NS, "-o", "json"])
        .expect("list secrets")
        .expect("a list is always present");
    assert!(
        listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["metadata"]["name"] == json!("pg-0-conn") && s["kind"] == json!("Secret")),
        "the smoke Secret must be listed, type-stamped: {listed}"
    );

    // --- get_secret_key: transport-decoded bytes -----------------------------
    assert_eq!(
        k.get_secret_key("pg-0-conn", NS, "pass").unwrap(),
        "s3cr3t/pw"
    );

    // --- apply_and_wait_pod_ready: SSA + readiness poll ----------------------
    let image = std::env::var("APPRAFTER_SMOKE_IMAGE")
        .unwrap_or_else(|_| "docker.io/library/alpine:3.24".to_string());
    k.apply_and_wait_pod_ready(&json!({
        "apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": POD, "namespace": NS},
        "spec": {
            "terminationGracePeriodSeconds": 0,
            "containers": [{
                "name": "helper", "image": image, "imagePullPolicy": "IfNotPresent",
                "command": ["sleep", "3600"]
            }]
        }
    }))
    .expect("helper pod reaches Ready");

    // --- exec over the WebSocket: stdout -> file, 1 MiB + a header ------------
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("dump");
    k.exec_stream_to_file(
        POD,
        NS,
        &[
            "sh",
            "-c",
            "printf 'hello\\n'; head -c 1048576 /dev/zero | tr '\\0' x",
        ],
        &out,
        // Bounded, so the timed first read and the copy after it both run
        // against a real stream: the whole of it must still arrive.
        Some(std::time::Duration::from_secs(60)),
    )
    .expect("exec_stream_to_file");
    let got = std::fs::read(&out).unwrap();
    assert_eq!(got.len(), 6 + 1_048_576, "the whole stream must arrive");
    assert!(got.starts_with(b"hello\n") && got[6..].iter().all(|b| *b == b'x'));

    // --- exec: a non-zero exit is an error carrying the apiserver's Status ----
    let err = k
        .exec_stream_to_file(
            POD,
            NS,
            &["sh", "-c", "exit 3"],
            &dir.path().join("x"),
            None,
        )
        .expect_err("a non-zero exit must fail the step");
    let msg = format!("{err}");
    assert!(
        msg.contains("NonZeroExitCode"),
        "the remotecommand Status reason must reach the error: {msg}"
    );

    // --- write_status: create on a real 404, then merge onto the live CM -------
    rt.block_on(write_status(
        &client,
        &RunOutcome::Failure {
            error: "boom".into(),
        },
        "monolithic",
        "2026-09-22T17:53:30Z",
    ))
    .expect("status CM create");
    rt.block_on(write_status(
        &client,
        &RunOutcome::Success { snapshot: None },
        "monolithic",
        "2026-09-22T17:54:30Z",
    ))
    .expect("status CM merge");
    let cm = rt
        .block_on(Api::<ConfigMap>::namespaced(client.clone(), STATUS_NS).get(STATUS_CM))
        .expect("read the status CM back");
    let data = cm.data.expect("status CM data");
    assert_eq!(data["lastFailure"], "2026-09-22T17:53:30Z");
    assert_eq!(data["lastSuccess"], "2026-09-22T17:54:30Z");
    assert_eq!(data["lastError"], "", "a success clears the stale error");

    // --- delete_pod_best_effort: issues the delete ------------------------------
    k.delete_pod_best_effort(POD, NS);
    let pod = k
        .get_json(&["get", "pods", POD, "-n", NS, "-o", "json"])
        .expect("get the helper pod after delete");
    assert!(
        pod.as_ref()
            .is_none_or(|p| p["metadata"]["deletionTimestamp"].is_string()),
        "the helper pod must be gone or terminating: {pod:?}"
    );
    // A second delete of a gone pod must be silent.
    std::thread::sleep(Duration::from_millis(200));
    k.delete_pod_best_effort(POD, NS);

    // --- cleanup --------------------------------------------------------------
    rt.block_on(async {
        let _ = Api::<ConfigMap>::namespaced(client.clone(), STATUS_NS)
            .delete(STATUS_CM, &DeleteParams::default())
            .await;
        let _ = Api::<Namespace>::all(client.clone())
            .delete(NS, &DeleteParams::default())
            .await;
    });
}
