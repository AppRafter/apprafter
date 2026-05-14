// SPDX-License-Identifier: FSL-1.1-MIT
//! Manual smoke test for the v0.1.11/v0.1.12 cluster-bootstrap
//! outcome: cilium-status зелёный, Gateway API admission works,
//! default-deny NetworkPolicy is in place. Skipped by default —
//! opt in with:
//!
//!     APPRAFTER_K8S_SMOKE=1 KUBECONFIG=… \
//!     cargo test -p apprafter --test cluster_smoke_test \
//!         smoke -- --ignored
//!
//! Talks to the system `cilium` and `kubectl` binaries directly;
//! does NOT exercise apprafter (the v0.1.11 + v0.1.12 unit tests
//! already prove the orchestration shape).

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
#[ignore = "real cluster — set APPRAFTER_K8S_SMOKE=1 + KUBECONFIG to run"]
fn smoke_cilium_gateway_default_deny() {
    if std::env::var("APPRAFTER_K8S_SMOKE").as_deref() != Ok("1") {
        eprintln!("skip: APPRAFTER_K8S_SMOKE is not set to 1");
        return;
    }
    if std::env::var("KUBECONFIG").is_err() {
        panic!("KUBECONFIG must be set to a working cluster");
    }

    // 1. cilium status — exit 0 implies all agents healthy.
    let st = Command::new("cilium")
        .arg("status")
        .arg("--wait")
        .arg("--wait-duration")
        .arg("60s")
        .status()
        .expect("spawn cilium");
    assert!(st.success(), "cilium status failed (exit {st:?})");

    // 2. Gateway API admission: a minimal Gateway must pass
    //    `--dry-run=server` validation. Use stdin to avoid leaving
    //    a manifest file on the operator's disk.
    let gateway = "apiVersion: gateway.networking.k8s.io/v1\n\
kind: Gateway\n\
metadata:\n\
  name: smoke-gw\n\
  namespace: default\n\
spec:\n\
  gatewayClassName: cilium\n\
  listeners:\n\
    - name: http\n\
      port: 80\n\
      protocol: HTTP\n";

    let mut child = Command::new("kubectl")
        .args(["apply", "--dry-run=server", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn kubectl");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(gateway.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("kubectl finish");
    assert!(
        out.status.success(),
        "kubectl apply --dry-run=server Gateway failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // 3. default-deny NetworkPolicy is in place in `default`.
    let st = Command::new("kubectl")
        .args(["get", "networkpolicy", "default-deny", "-n", "default"])
        .status()
        .expect("spawn kubectl");
    assert!(
        st.success(),
        "default-deny NetworkPolicy not found in `default` namespace"
    );

    // 4. Bootstrap Application present iff caller opts in. We gate
    //    on a second env var so smoke runs against a v0.1.16
    //    cluster (no bootstrap-Application) don't fail.
    if std::env::var("APPRAFTER_BOOTSTRAP_REPO_SMOKE").as_deref() == Ok("1") {
        let st = Command::new("kubectl")
            .args(["get", "application", "bootstrap", "-n", "argocd"])
            .status()
            .expect("spawn kubectl");
        assert!(
            st.success(),
            "bootstrap Application not found in argocd namespace"
        );
    } else {
        eprintln!(
            "skip: APPRAFTER_BOOTSTRAP_REPO_SMOKE not set; not asserting bootstrap Application"
        );
    }
}
