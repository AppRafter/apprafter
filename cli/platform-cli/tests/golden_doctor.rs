// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Golden snapshots of `apprafter doctor` (D.3a baselines). The harness and its rules are in
//! `common/golden.rs`; the tool rows run against GOTCHA-66 stand-ins or an empty `PATH`.
#![cfg(unix)]

mod common;
use common::golden::*;

#[test]
fn doctor_without_a_target_on_an_empty_path() {
    Sandbox::new().golden("doctor/no_target_empty_path", &["doctor"]);
}

#[test]
fn doctor_with_stand_in_tools_and_a_verified_token() {
    let mut server = mockito::Server::new();
    let _loc = json_mock(&mut server, "/v1/locations", 200, LOCATIONS_OK, TOKEN_A);
    let sb = Sandbox::new()
        .with_hcloud(server.url())
        .with_stand_in_tools();
    sb.add_target("prod");
    sb.golden("doctor/target_tools_ping_ok", &["doctor"]);
}

#[test]
fn doctor_with_a_target_on_an_empty_path() {
    // The token ping hits the closed port.
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.golden("doctor/target_empty_path", &["doctor"]);
}

#[test]
fn doctor_with_stand_in_tools_and_no_ping() {
    let sb = Sandbox::new().with_stand_in_tools();
    sb.add_target("prod");
    sb.golden("doctor/target_tools_no_ping", &["doctor", "--no-ping"]);
}

#[test]
fn doctor_of_a_target_that_does_not_exist() {
    let sb = Sandbox::new().with_stand_in_tools();
    sb.add_target("prod");
    sb.golden(
        "doctor/target_ghost",
        &["doctor", "--target", "ghost", "--no-ping"],
    );
}

/// D.3d review #5: doctor's row for a credentials file that does not parse names the file and
/// where in it; serde's own text would quote the token (`hetzner_token:<token>`, no space, is
/// one scalar).
#[test]
fn doctor_with_credentials_missing_the_space_after_the_colon() {
    let sb = Sandbox::new().with_stand_in_tools();
    sb.add_target("prod");
    sb.seed_store_file(
        "targets/prod/credentials.yaml",
        &format!("hetzner_token:{TOKEN_A}\n"),
    );
    let args = ["doctor", "--no-ping"];
    sb.assert_steps_never_print(&[&args], TOKEN_A);
    sb.golden("doctor/credentials_no_space", &args);
}

#[test]
fn doctor_with_a_rejected_token() {
    let mut server = mockito::Server::new();
    let _loc = json_mock(&mut server, "/v1/locations", 401, UNAUTHORIZED, TOKEN_A);
    let sb = Sandbox::new()
        .with_hcloud(server.url())
        .with_stand_in_tools();
    sb.add_target("prod");
    sb.golden("doctor/token_rejected", &["doctor"]);
}

#[test]
fn doctor_of_a_provisioned_target() {
    let sb = Sandbox::new().with_stand_in_tools();
    sb.add_target("prod");
    sb.seed_state("prod", PROVISIONED_STATE);
    sb.golden("doctor/provisioned", &["doctor", "--no-ping"]);
}

#[test]
fn doctor_with_a_dangling_pointer() {
    let sb = Sandbox::new().with_stand_in_tools();
    sb.add_target("prod");
    sb.seed_pointer("gone");
    sb.golden("doctor/dangling_pointer", &["doctor", "--no-ping"]);
}

// ---- the Cluster group (D.3c Task 8). The node SSH row is in no golden on purpose: without
// `--no-ping` it dials port 22 of whatever address the mock returns, and whether the machine
// running the suite has sshd is not a property of this code. The core's tests cover it with a
// local listener.

const KUBECONFIG_YAML: &str = "apiVersion: v1\nkind: Config\nclusters:\n- name: c\n  cluster:\n    server: https://127.0.0.1:6443\ncontexts:\n- name: c\n  context:\n    cluster: c\n    user: u\ncurrent-context: c\nusers:\n- name: u\n  user:\n    token: golden\n";

fn provisioned(slot: Option<(&str, String)>) -> serde_json::Value {
    let mut server = serde_json::json!({"server_id": 7, "server_name": "apprafter-prod"});
    if let Some((key, value)) = slot {
        server[key] = serde_json::json!(value);
    }
    serde_json::json!({ "hetzner_cloud": server })
}

/// The age key at the sandbox's default key path (HOME is `<sandbox>/home`), and the
/// kubeconfig encrypted to it.
fn encrypted_kubeconfig(sb: &Sandbox) -> String {
    let id = cli_core::secrets::load_or_create_identity(&sb.path("home/.config/apprafter/age.key"))
        .expect("age key");
    cli_core::secrets::encrypt_for_recipient(KUBECONFIG_YAML, &id.to_public()).expect("encrypt")
}

/// `<sandbox>/bin/kubectl` — on the harness's whole `PATH` — answering `version --client` as
/// kubectl does and anything else with `body`.
fn fake_kubectl(sb: &Sandbox, body: &str) {
    common::stand_in::script(
        &sb.path("bin/kubectl"),
        &format!("case \"$1\" in version) echo 'Client Version: v1.31.0'; exit 0 ;; esac\n{body}"),
    );
}

fn no_runtime_kubeconfig_left(sb: &Sandbox) {
    if let Ok(rd) = std::fs::read_dir(sb.path("apprafter-config/run")) {
        let left: Vec<String> = rd
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            left.iter().all(|n| !n.starts_with("kubeconfig-")),
            "left behind: {left:?}"
        );
    }
}

#[test]
fn doctor_cluster_legacy_plaintext() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.seed_state(
        "prod",
        &provisioned(Some(("kubeconfig_yaml", KUBECONFIG_YAML.to_string()))).to_string(),
    );
    sb.golden("doctor/cluster_legacy_plaintext", &["doctor", "--no-ping"]);
}

#[test]
fn doctor_cluster_not_cached() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    sb.seed_state("prod", &provisioned(None).to_string());
    sb.golden("doctor/cluster_not_cached", &["doctor", "--no-ping"]);
}

#[test]
fn doctor_cluster_age_key_missing() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    let armored =
        "-----BEGIN AGE ENCRYPTED FILE-----\n-----END AGE ENCRYPTED FILE-----\n".to_string();
    sb.seed_state(
        "prod",
        &provisioned(Some(("kubeconfig_age", armored))).to_string(),
    );
    sb.golden("doctor/cluster_age_key_missing", &["doctor", "--no-ping"]);
    assert!(
        !sb.path("home/.config/apprafter/age.key").exists(),
        "doctor must never create an age key"
    );
}

#[test]
fn doctor_cluster_api_ok() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    let armored = encrypted_kubeconfig(&sb);
    sb.seed_state(
        "prod",
        &provisioned(Some(("kubeconfig_age", armored))).to_string(),
    );
    fake_kubectl(
        &sb,
        r#"printf '%s' '{"major":"1","minor":"31","gitVersion":"v1.31.0+k3s1"}'"#,
    );
    sb.golden("doctor/cluster_api_ok", &["doctor", "--no-ping"]);
    no_runtime_kubeconfig_left(&sb);
}

#[test]
fn doctor_cluster_api_unreachable() {
    let sb = Sandbox::new();
    sb.add_target("prod");
    let armored = encrypted_kubeconfig(&sb);
    sb.seed_state(
        "prod",
        &provisioned(Some(("kubeconfig_age", armored))).to_string(),
    );
    fake_kubectl(
        &sb,
        "echo 'Unable to connect to the server: dial tcp 127.0.0.1:6443: connect: connection refused' >&2\nexit 1",
    );
    sb.golden("doctor/cluster_api_unreachable", &["doctor", "--no-ping"]);
    no_runtime_kubeconfig_left(&sb);
}
