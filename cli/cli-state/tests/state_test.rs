// SPDX-License-Identifier: FSL-1.1-Apache-2.0
use cli_core::Tier;
use cli_state::{HetznerCloudState, State, StatePaths};

#[test]
fn load_or_default_returns_default_on_fresh_dir() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_root(dir.path());

    let state = State::load_or_default(&paths).unwrap();
    assert_eq!(state, State::default());
    assert_eq!(state.tier, None);
}

#[test]
fn save_then_load_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_root(dir.path());

    let original = State {
        cluster_name: Some("solo-1".to_string()),
        tier: Some(Tier::Solo),
        ..Default::default()
    };
    original.save(&paths).unwrap();

    let reloaded = State::load_or_default(&paths).unwrap();
    assert_eq!(reloaded, original);
}

#[test]
fn load_rejects_corrupt_state_file() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_root(dir.path());
    std::fs::create_dir_all(paths.state_file().parent().unwrap()).unwrap();
    std::fs::write(paths.state_file(), b"{not json}").unwrap();

    let err = State::load_or_default(&paths).unwrap_err();
    assert!(
        matches!(err, cli_core::CliError::InvalidState { .. }),
        "got: {err}"
    );
}

#[test]
fn hetzner_cloud_state_round_trips_through_save_load() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_root(dir.path());

    let original = State {
        cluster_name: Some("solo-1".into()),
        tier: Some(Tier::Solo),
        provider: Some("hetzner-cloud".into()),
        region: Some("nbg1".into()),
        hetzner_cloud: Some(cli_state::HetznerCloudState {
            server_id: 12345,
            server_name: "platform-1".into(),
            server_type: None,
            ssh_key_ids: vec![],
            network_id: None,
            firewall_id: None,
            floating_ip_ids: vec![],
            kubeconfig_yaml: None,
            kubeconfig_age: None,
            argocd_admin_password_age: None,
            kubeconfig_sealed_to: None,
            argocd_admin_password_sealed_to: None,
        }),
    };
    original.save(&paths).unwrap();
    let reloaded = State::load_or_default(&paths).unwrap();
    assert_eq!(reloaded, original);
}

#[test]
fn hetzner_cloud_state_carries_ssh_key_ids() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_root(dir.path());

    let original = State {
        cluster_name: Some("solo-1".into()),
        tier: Some(Tier::Solo),
        provider: Some("hetzner-cloud".into()),
        region: Some("nbg1".into()),
        hetzner_cloud: Some(cli_state::HetznerCloudState {
            server_id: 42,
            server_name: "platform-1".into(),
            server_type: None,
            ssh_key_ids: vec![7, 9],
            network_id: None,
            firewall_id: None,
            floating_ip_ids: vec![],
            kubeconfig_yaml: None,
            kubeconfig_age: None,
            argocd_admin_password_age: None,
            kubeconfig_sealed_to: None,
            argocd_admin_password_sealed_to: None,
        }),
    };
    original.save(&paths).unwrap();
    let reloaded = State::load_or_default(&paths).unwrap();
    assert_eq!(reloaded, original);
}

#[test]
fn hetzner_cloud_state_carries_network_and_firewall_ids() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_root(dir.path());

    let original = State {
        cluster_name: Some("solo-1".into()),
        tier: Some(Tier::Solo),
        provider: Some("hetzner-cloud".into()),
        region: Some("nbg1".into()),
        hetzner_cloud: Some(cli_state::HetznerCloudState {
            server_id: 42,
            server_name: "platform-1".into(),
            server_type: None,
            ssh_key_ids: vec![7],
            network_id: Some(11),
            firewall_id: Some(21),
            floating_ip_ids: vec![],
            kubeconfig_yaml: None,
            kubeconfig_age: None,
            argocd_admin_password_age: None,
            kubeconfig_sealed_to: None,
            argocd_admin_password_sealed_to: None,
        }),
    };
    original.save(&paths).unwrap();
    let reloaded = State::load_or_default(&paths).unwrap();
    assert_eq!(reloaded, original);
}

#[test]
fn hetzner_cloud_state_carries_floating_ip_ids() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_root(dir.path());

    let original = State {
        cluster_name: Some("solo-1".into()),
        tier: Some(Tier::Solo),
        provider: Some("hetzner-cloud".into()),
        region: Some("nbg1".into()),
        hetzner_cloud: Some(cli_state::HetznerCloudState {
            server_id: 42,
            server_name: "platform-1".into(),
            server_type: None,
            ssh_key_ids: vec![7],
            network_id: Some(11),
            firewall_id: Some(21),
            floating_ip_ids: vec![31, 32],
            kubeconfig_yaml: None,
            kubeconfig_age: None,
            argocd_admin_password_age: None,
            kubeconfig_sealed_to: None,
            argocd_admin_password_sealed_to: None,
        }),
    };
    original.save(&paths).unwrap();
    let reloaded = State::load_or_default(&paths).unwrap();
    assert_eq!(reloaded, original);
}

#[test]
fn hetzner_cloud_state_round_trips_cached_kubeconfig_yaml() {
    use cli_state::HetznerCloudState;

    let s = HetznerCloudState {
        server_id: 42,
        server_name: "platform-1".into(),
        server_type: None,
        ssh_key_ids: vec![],
        network_id: None,
        firewall_id: None,
        floating_ip_ids: vec![],
        kubeconfig_yaml: Some("apiVersion: v1\n".into()),
        kubeconfig_age: None,
        argocd_admin_password_age: None,
        kubeconfig_sealed_to: None,
        argocd_admin_password_sealed_to: None,
    };
    let json = serde_json::to_string(&s).unwrap();
    let back: HetznerCloudState = serde_json::from_str(&json).unwrap();
    assert_eq!(back.kubeconfig_yaml.as_deref(), Some("apiVersion: v1\n"));
}

#[test]
fn hetzner_cloud_state_kubeconfig_yaml_defaults_to_none_when_absent() {
    use cli_state::HetznerCloudState;
    let json = r#"{
        "server_id": 1, "server_name": "n",
        "ssh_key_ids": [], "network_id": null,
        "firewall_id": null, "floating_ip_ids": []
    }"#;
    let s: HetznerCloudState = serde_json::from_str(json).unwrap();
    assert!(s.kubeconfig_yaml.is_none());
}

#[test]
fn hetzner_cloud_state_round_trips_cached_kubeconfig_age() {
    use cli_state::HetznerCloudState;

    let s = HetznerCloudState {
        server_id: 42,
        server_name: "platform-1".into(),
        server_type: None,
        ssh_key_ids: vec![],
        network_id: None,
        firewall_id: None,
        floating_ip_ids: vec![],
        kubeconfig_yaml: None,
        kubeconfig_age: Some(
            "-----BEGIN AGE ENCRYPTED FILE-----\nfake\n-----END AGE ENCRYPTED FILE-----\n".into(),
        ),
        argocd_admin_password_age: None,
        kubeconfig_sealed_to: None,
        argocd_admin_password_sealed_to: None,
    };
    let json = serde_json::to_string(&s).unwrap();
    let back: HetznerCloudState = serde_json::from_str(&json).unwrap();
    assert_eq!(
        back.kubeconfig_age.as_deref(),
        Some("-----BEGIN AGE ENCRYPTED FILE-----\nfake\n-----END AGE ENCRYPTED FILE-----\n")
    );
    assert!(back.kubeconfig_yaml.is_none());
}

#[test]
fn hetzner_cloud_state_kubeconfig_age_defaults_to_none_when_absent() {
    use cli_state::HetznerCloudState;
    let json = r#"{
        "server_id": 1, "server_name": "n",
        "ssh_key_ids": [], "network_id": null,
        "firewall_id": null, "floating_ip_ids": [],
        "kubeconfig_yaml": null
    }"#;
    let s: HetznerCloudState = serde_json::from_str(json).unwrap();
    assert!(s.kubeconfig_age.is_none());
}

#[test]
fn hetzner_cloud_state_round_trips_argocd_admin_password_age() {
    use cli_state::HetznerCloudState;

    let s = HetznerCloudState {
        server_id: 42,
        server_name: "platform-1".into(),
        server_type: None,
        ssh_key_ids: vec![],
        network_id: None,
        firewall_id: None,
        floating_ip_ids: vec![],
        kubeconfig_yaml: None,
        kubeconfig_age: None,
        argocd_admin_password_age: Some(
            "-----BEGIN AGE ENCRYPTED FILE-----\nfake\n-----END AGE ENCRYPTED FILE-----\n".into(),
        ),
        kubeconfig_sealed_to: None,
        argocd_admin_password_sealed_to: None,
    };
    let json = serde_json::to_string(&s).unwrap();
    let back: HetznerCloudState = serde_json::from_str(&json).unwrap();
    assert!(back.argocd_admin_password_age.is_some());
    assert!(back
        .argocd_admin_password_age
        .as_deref()
        .unwrap()
        .contains("AGE ENCRYPTED FILE"));
}

#[test]
fn hetzner_cloud_state_argocd_admin_password_age_defaults_to_none_when_absent() {
    let json = r#"{
        "server_id": 1, "server_name": "n",
        "ssh_key_ids": [], "network_id": null,
        "firewall_id": null, "floating_ip_ids": [],
        "kubeconfig_yaml": null, "kubeconfig_age": null
    }"#;
    let s: HetznerCloudState = serde_json::from_str(json).unwrap();
    assert!(s.argocd_admin_password_age.is_none());
}

// ── server_type field (M28 / Task 11) ─────────────────────────────────────────

#[test]
fn hetzner_cloud_state_round_trips_server_type() {
    // Serialize with `server_type: Some("ccx23")` and verify it survives
    // a serde round-trip unchanged.
    let s = HetznerCloudState {
        server_id: 42,
        server_name: "platform-1".into(),
        server_type: Some("ccx23".into()),
        ssh_key_ids: vec![],
        network_id: None,
        firewall_id: None,
        floating_ip_ids: vec![],
        kubeconfig_yaml: None,
        kubeconfig_age: None,
        argocd_admin_password_age: None,
        kubeconfig_sealed_to: None,
        argocd_admin_password_sealed_to: None,
    };
    let json = serde_json::to_string(&s).unwrap();
    let back: HetznerCloudState = serde_json::from_str(&json).unwrap();
    assert_eq!(back.server_type.as_deref(), Some("ccx23"));
    assert_eq!(back.server_id, 42);
}

#[test]
fn hetzner_cloud_state_server_type_without_it_is_none() {
    // Serialize without `server_type` (it is `None`) and verify round-trip
    // keeps it as `None`.
    let s = HetznerCloudState {
        server_id: 7,
        server_name: "platform-1".into(),
        server_type: None,
        ssh_key_ids: vec![],
        network_id: None,
        firewall_id: None,
        floating_ip_ids: vec![],
        kubeconfig_yaml: None,
        kubeconfig_age: None,
        argocd_admin_password_age: None,
        kubeconfig_sealed_to: None,
        argocd_admin_password_sealed_to: None,
    };
    let json = serde_json::to_string(&s).unwrap();
    let back: HetznerCloudState = serde_json::from_str(&json).unwrap();
    assert!(back.server_type.is_none());
}

#[test]
fn hetzner_cloud_state_server_type_defaults_to_none_on_old_json() {
    // Old `state.json` files that predate the `server_type` field must
    // still deserialize cleanly (serde-default back-compat).
    let old_json = r#"{
        "server_id": 99, "server_name": "legacy",
        "ssh_key_ids": [5], "network_id": null,
        "firewall_id": null, "floating_ip_ids": []
    }"#;
    let s: HetznerCloudState = serde_json::from_str(old_json).unwrap();
    assert!(
        s.server_type.is_none(),
        "old JSON without server_type must deserialize to None, got {:?}",
        s.server_type
    );
    assert_eq!(s.server_id, 99);
    assert_eq!(s.ssh_key_ids, vec![5]);
}

/// F13: what sealed each cache, when not the shared age key, round-trips through `save`/`load`
/// as `{"kind": "store_vault", "store_id": …}`; unset it is not written at all, so an older
/// reader sees the file it knows.
#[test]
fn what_sealed_a_cache_round_trips_and_is_absent_when_unset() {
    use cli_core::secrets::CachedSecret;
    use cli_state::SealedTo;
    let dir = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_root(dir.path());
    let vault = SealedTo::StoreVault {
        store_id: "3f9c0a1b7d2e4c56".into(),
    };
    let mut h: HetznerCloudState =
        serde_json::from_str(r#"{"server_id": 1, "server_name": "n"}"#).unwrap();
    *h.cache_mut(CachedSecret::Kubeconfig).0 = Some("K".into());
    *h.cache_mut(CachedSecret::Kubeconfig).1 = Some(vault.clone());
    let state = State {
        hetzner_cloud: Some(h),
        ..Default::default()
    };
    state.save(&paths).unwrap();
    let raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(paths.state_file()).unwrap()).unwrap();
    assert_eq!(
        raw["hetzner_cloud"]["kubeconfig_sealed_to"],
        serde_json::json!({"kind": "store_vault", "store_id": "3f9c0a1b7d2e4c56"})
    );
    assert!(
        raw["hetzner_cloud"]
            .get("argocd_admin_password_sealed_to")
            .is_none(),
        "{raw}"
    );
    let back = State::load_or_default(&paths).unwrap();
    assert_eq!(back, state);
    let h = back.hetzner_cloud.unwrap();
    assert_eq!(h.sealed_to(CachedSecret::Kubeconfig), Some(&vault));
    assert_eq!(h.sealed_to(CachedSecret::ArgocdPassword), None);
}

/// F13: a state.json written before the record (or by an older AppRafter) still parses, with
/// nothing recorded.
#[test]
fn a_state_without_the_seal_record_still_parses() {
    let old: HetznerCloudState =
        serde_json::from_str(r#"{"server_id": 1, "server_name": "n", "kubeconfig_age": "K"}"#)
            .unwrap();
    assert_eq!(old.kubeconfig_sealed_to, None);
    assert_eq!(old.argocd_admin_password_sealed_to, None);
    let empty: State = serde_json::from_str("{}").unwrap();
    assert_eq!(empty, State::default());
}
