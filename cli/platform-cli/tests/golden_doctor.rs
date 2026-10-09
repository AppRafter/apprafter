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
