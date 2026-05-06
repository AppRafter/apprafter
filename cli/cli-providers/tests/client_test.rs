// SPDX-License-Identifier: FSL-1.1-MIT
//! HTTP-level tests for HetznerCloudClient using mockito.

use cli_core::CliError;
use cli_providers::hetzner_cloud::HetznerCloudClient;

#[test]
fn get_servers_includes_bearer_header_and_decodes_list() {
    let mut server = mockito::Server::new();
    let m = server
        .mock("GET", "/v1/servers")
        .match_header("Authorization", "Bearer test-token")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"servers":[{"id":1,"name":"a","status":"running","labels":{}}]}"#)
        .create();

    let client = HetznerCloudClient::new(server.url(), "test-token");
    let resp = client.list_servers().expect("list_servers should succeed");
    assert_eq!(resp.servers.len(), 1);
    assert_eq!(resp.servers[0].id, 1);
    m.assert();
}

#[test]
fn http_error_maps_to_cli_error_hetzner() {
    let mut server = mockito::Server::new();
    server
        .mock("GET", "/v1/servers")
        .with_status(401)
        .with_header("content-type", "application/json")
        .with_body(r#"{"error":{"code":"unauthorized","message":"bad token"}}"#)
        .create();

    let client = HetznerCloudClient::new(server.url(), "wrong");
    let err = client.list_servers().unwrap_err();
    match err {
        CliError::Hetzner {
            status,
            code,
            message,
            ..
        } => {
            assert_eq!(status, 401);
            assert_eq!(code, "unauthorized");
            assert_eq!(message, "bad token");
        }
        other => panic!("expected Hetzner error, got {other:?}"),
    }
}
