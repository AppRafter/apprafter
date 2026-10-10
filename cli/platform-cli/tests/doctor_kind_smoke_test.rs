// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Doctor's Cluster group against a real API server: a DISPOSABLE kind cluster, never the
//! owner's. The cached kubeconfig is the kind one, age-encrypted under a scratch key in a
//! scratch store; the child runs with `KUBECONFIG=/nonexistent`, so a PASS proves the probe
//! used the materialised copy and nothing ambient. A second target with the same kubeconfig,
//! its current context's cluster pointed at a closed port, proves the real kubectl's refusal
//! is classified.
//!
//! ```text
//! APPRAFTER_K8S_SMOKE=1 KUBECONFIG=<kind kubeconfig> \
//!     cargo test -p apprafter --test doctor_kind_smoke_test -- --ignored
//! ```
//!
//! Refuses any kubeconfig that names a context other than `kind-*`, current or not, or a
//! cluster whose server is not on this machine (kind's API servers listen on loopback). Needs
//! a real `kubectl` on `PATH` (the nix dev shell has one). A missing precondition is a failure,
//! never a skip.
#![cfg(unix)]

use std::path::Path;

use assert_cmd::Command;
use serde_yaml::Value;

const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// Where the "closed" target's cluster points: nothing listens on port 1.
const CLOSED: &str = "https://127.0.0.1:1";

/// Refuse (panic on) a kubeconfig this test must not cache: one whose current context, or any
/// context, is not `kind-*` (the whole file is cached and decrypted, so it must hold kind
/// clusters and nothing else), or one with a cluster whose server is not on this machine.
fn refuse_all_but_kind(doc: &Value) {
    let context = doc["current-context"].as_str().unwrap_or_default();
    assert!(
        context.starts_with("kind-"),
        "refusing to run against context {context:?}: kind clusters only"
    );
    let contexts: Vec<&str> = doc["contexts"]
        .as_sequence()
        .map(|s| s.iter().filter_map(|c| c["name"].as_str()).collect())
        .unwrap_or_default();
    assert!(
        !contexts.is_empty() && contexts.iter().all(|c| c.starts_with("kind-")),
        "refusing a kubeconfig with contexts {contexts:?}: kind clusters only"
    );
    for cluster in doc["clusters"].as_sequence().into_iter().flatten() {
        let server = cluster["cluster"]["server"].as_str().unwrap_or_default();
        assert!(
            on_this_machine(server),
            "refusing cluster {:?} at {server:?}: not on this machine, kind clusters only",
            cluster["name"]
        );
    }
}

/// Whether `server` (`https://host:port[/path]`) names a loopback host.
fn on_this_machine(server: &str) -> bool {
    let rest = server.split_once("://").map_or(server, |(_, r)| r);
    let authority = rest.split('/').next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority.rsplit_once(':').map_or(authority, |(h, _)| h),
    };
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// `doc` as YAML, with the cluster of its CURRENT context pointed at [`CLOSED`]: kubectl, given
/// the file and no `--context`, talks to that cluster, which need not be the first listed (kind
/// appends each new cluster and makes it current).
fn with_current_cluster_closed(doc: &Value) -> String {
    let context = doc["current-context"].as_str().unwrap_or_default();
    let cluster = doc["contexts"]
        .as_sequence()
        .and_then(|cs| cs.iter().find(|c| c["name"].as_str() == Some(context)))
        .and_then(|c| c["context"]["cluster"].as_str())
        .unwrap_or_else(|| panic!("the current context {context:?} names no cluster"))
        .to_string();
    let mut closed = doc.clone();
    let entry = closed["clusters"]
        .as_sequence_mut()
        .and_then(|cs| cs.iter_mut().find(|c| c["name"].as_str() == Some(&cluster)))
        .unwrap_or_else(|| panic!("no cluster {cluster:?} in the kubeconfig"));
    entry["cluster"]["server"] = Value::from(CLOSED);
    serde_yaml::to_string(&closed).unwrap()
}

fn apprafter(config: &Path, key: &Path) -> Command {
    let mut c = Command::cargo_bin("apprafter").unwrap();
    c.env("APPRAFTER_SKIP_STARTUP_CHECKS", "1")
        .env("APPRAFTER_CONFIG_DIR", config)
        .env("APPRAFTER_AGE_KEY", key)
        .env("APPRAFTER_HCLOUD_BASE_URL", "http://127.0.0.1:1")
        .env("KUBECONFIG", "/nonexistent")
        .env_remove("HCLOUD_TOKEN");
    c
}

/// A target whose state records a server and caches `kubeconfig`, encrypted to `key`.
fn add_target(config: &Path, key: &Path, name: &str, kubeconfig: &str) {
    apprafter(config, key)
        .args([
            "target",
            "add",
            name,
            "--provider",
            "hetzner-cloud",
            "--token",
            TOKEN,
            "--no-ping",
            "--no-interactive",
        ])
        .assert()
        .success();
    let id = cli_core::secrets::load_or_create_identity(key).unwrap();
    let armored = cli_core::secrets::encrypt_for_recipient(kubeconfig, &id.to_public()).unwrap();
    let state = config.join("state").join(name).join(".apprafter");
    std::fs::create_dir_all(&state).unwrap();
    let body = serde_json::json!({"hetzner_cloud": {"server_id": 1, "server_name": name, "kubeconfig_age": armored}});
    std::fs::write(state.join("state.json"), body.to_string()).unwrap();
}

#[test]
#[ignore = "real cluster — set APPRAFTER_K8S_SMOKE=1 + KUBECONFIG (a kind cluster) to run"]
fn doctor_reaches_a_kind_cluster_through_its_cached_kubeconfig_only() {
    assert_eq!(
        std::env::var("APPRAFTER_K8S_SMOKE").as_deref(),
        Ok("1"),
        "run with APPRAFTER_K8S_SMOKE=1 (this test talks to a cluster)"
    );
    let kind = std::env::var_os("KUBECONFIG")
        .expect("KUBECONFIG must name the kind cluster's kubeconfig explicitly");
    let yaml = std::fs::read_to_string(&kind).expect("read the kind kubeconfig");
    let doc: Value = serde_yaml::from_str(&yaml).unwrap();
    refuse_all_but_kind(&doc);

    let scratch = tempfile::tempdir().unwrap();
    let config = scratch.path().join("config");
    let key = scratch.path().join("age.key");
    add_target(&config, &key, "kind", &yaml);
    add_target(&config, &key, "closed", &with_current_cluster_closed(&doc));

    let out = apprafter(&config, &key)
        .args(["doctor", "--target", "kind", "--no-ping"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    println!("{stdout}");
    assert!(
        stdout.contains("  ✓ Kubeconfig cached (encrypted)"),
        "{stdout}"
    );
    let api = stdout
        .lines()
        .find(|l| l.contains("Kube API reachable"))
        .unwrap_or_default();
    assert!(
        api.starts_with("  ✓ Kube API reachable (v1.") && api.ends_with(" ms)"),
        "{stdout}"
    );

    let out = apprafter(&config, &key)
        .args(["doctor", "--target", "closed", "--no-ping"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    println!("{stdout}");
    assert!(stdout.contains("  ✗ Kube API reachable ("), "{stdout}");
    assert!(
        stdout.contains("apiserver did not answer"),
        "the refusal must classify as unreachable:\n{stdout}"
    );
    assert_eq!(out.status.code(), Some(1));

    let run = config.join("run");
    if let Ok(rd) = std::fs::read_dir(&run) {
        let left: Vec<_> = rd.map(|e| e.unwrap().file_name()).collect();
        assert!(
            left.is_empty(),
            "decrypted kubeconfigs left in {}: {left:?}",
            run.display()
        );
    }
}

/// A merged kubeconfig as kind leaves it after `kind create cluster --name a`, then `--name b`:
/// both clusters, in that order, and `kind-b` current. No cluster is contacted.
const MERGED: &str = "\
apiVersion: v1
kind: Config
clusters:
- name: kind-a
  cluster:
    server: https://127.0.0.1:40001
- name: kind-b
  cluster:
    server: https://127.0.0.1:40002
contexts:
- name: kind-a
  context:
    cluster: kind-a
    user: kind-a
- name: kind-b
  context:
    cluster: kind-b
    user: kind-b
current-context: kind-b
users:
- name: kind-a
  user:
    token: a
- name: kind-b
  user:
    token: b
";

fn server_of<'a>(doc: &'a Value, cluster: &str) -> &'a str {
    doc["clusters"]
        .as_sequence()
        .and_then(|cs| cs.iter().find(|c| c["name"].as_str() == Some(cluster)))
        .and_then(|c| c["cluster"]["server"].as_str())
        .unwrap()
}

/// Review findings 3 and 11: the "closed" target closes the cluster kubectl talks to, the
/// current context's, not the first one listed; with `clusters[0]` closed instead, it still
/// reached `kind-b` and the smoke test failed for a bug that was not there.
#[test]
fn the_closed_copy_closes_the_current_contexts_cluster_only() {
    let doc: Value = serde_yaml::from_str(MERGED).unwrap();
    refuse_all_but_kind(&doc);
    let closed: Value = serde_yaml::from_str(&with_current_cluster_closed(&doc)).unwrap();
    assert_eq!(server_of(&closed, "kind-b"), CLOSED);
    assert_eq!(server_of(&closed, "kind-a"), "https://127.0.0.1:40001");
    assert_eq!(closed["current-context"].as_str(), Some("kind-b"));
}

#[test]
#[should_panic(expected = "kind clusters only")]
fn a_current_context_that_is_not_kind_is_refused() {
    let doc: Value =
        serde_yaml::from_str(&MERGED.replace("current-context: kind-b", "current-context: prod"))
            .unwrap();
    refuse_all_but_kind(&doc);
}

#[test]
#[should_panic(expected = "not on this machine")]
fn a_cluster_off_this_machine_is_refused() {
    let doc: Value = serde_yaml::from_str(
        &MERGED.replace("https://127.0.0.1:40001", "https://203.0.113.10:6443"),
    )
    .unwrap();
    refuse_all_but_kind(&doc);
}

#[test]
fn loopback_servers_are_on_this_machine() {
    for server in [
        "https://127.0.0.1:6443",
        "https://localhost:6443",
        "https://[::1]:6443",
        "https://127.0.0.1:6443/prefix",
    ] {
        assert!(on_this_machine(server), "{server}");
    }
    for server in [
        "https://203.0.113.10:6443",
        "https://kind.example:6443",
        "https://[2001:db8::1]:6443",
        "",
    ] {
        assert!(!on_this_machine(server), "{server}");
    }
}
