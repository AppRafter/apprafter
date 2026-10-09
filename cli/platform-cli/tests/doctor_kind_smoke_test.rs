// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Doctor's Cluster group against a real API server: a DISPOSABLE kind cluster, never the
//! owner's. The cached kubeconfig is the kind one, age-encrypted under a scratch key in a
//! scratch store; the child runs with `KUBECONFIG=/nonexistent`, so a PASS proves the probe
//! used the materialised copy and nothing ambient. A second target with the same kubeconfig
//! pointed at a closed port proves the real kubectl's refusal is classified.
//!
//! ```text
//! APPRAFTER_K8S_SMOKE=1 KUBECONFIG=<kind kubeconfig> \
//!     cargo test -p apprafter --test doctor_kind_smoke_test -- --ignored
//! ```
//!
//! Refuses any kubeconfig that names a context other than `kind-*`, current or not. Needs a
//! real `kubectl` on `PATH` (the nix dev shell has one). A missing precondition is a failure,
//! never a skip.
#![cfg(unix)]

use std::path::Path;

use assert_cmd::Command;

const TOKEN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

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
    let doc: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
    let context = doc["current-context"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        context.starts_with("kind-"),
        "refusing to run against context {context:?}: kind clusters only"
    );
    // The whole file is cached and decrypted: it must hold kind clusters and nothing else.
    let contexts: Vec<&str> = doc["contexts"]
        .as_sequence()
        .map(|s| s.iter().filter_map(|c| c["name"].as_str()).collect())
        .unwrap_or_default();
    assert!(
        !contexts.is_empty() && contexts.iter().all(|c| c.starts_with("kind-")),
        "refusing a kubeconfig with contexts {contexts:?}: kind clusters only"
    );
    let server = doc["clusters"][0]["cluster"]["server"]
        .as_str()
        .expect("a server URL")
        .to_string();

    let scratch = tempfile::tempdir().unwrap();
    let config = scratch.path().join("config");
    let key = scratch.path().join("age.key");
    add_target(&config, &key, "kind", &yaml);
    add_target(
        &config,
        &key,
        "closed",
        &yaml.replace(&server, "https://127.0.0.1:1"),
    );

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
