// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The Hetzner token crosses IPC once per attempt and never comes back (overview decision 6,
//! §6.3): verify → catalogue → add plan → execute, a renew, and a rejected token, against a
//! loopback mock of the API. Every reply the page gets, every operation event, plan view and
//! error, and the shell's `Debug` output — the draft store's while a draft waits — are searched
//! for the tokens; the mock checks each request carried the right one, so the path is the real
//! one.

// The rig's other helpers (the lock routes, the session watch) serve the other targets.
#[allow(dead_code)]
mod common;

use std::sync::Arc;

use apprafter_core::Outcome;
use apprafter_desktop_ipc::{errors, OpEvent, OpId};
use common::{code, follow_to_end, invoke, lock_off, rig_with_api, Recorder, Rig};
use serde_json::{json, Value};

const LOCATIONS_OK: &str = r#"{"locations":[
  {"id":1,"name":"fsn1","description":"Falkenstein DC Park 1","country":"DE","city":"Falkenstein","network_zone":"eu-central"},
  {"id":2,"name":"nbg1","description":"Nuremberg DC Park 1","country":"DE","city":"Nuremberg","network_zone":"eu-central"}]}"#;
const UNAUTHORIZED: &str =
    r#"{"error":{"code":"unauthorized","message":"unable to authenticate"}}"#;
/// cli/platform-cli/tests/common/golden.rs's cx22, sold in nbg1.
const SERVER_TYPES: &str = r#"{"server_types":[
  {"id":104,"name":"cx22","architecture":"x86","cpu_type":"shared","cores":2,"memory":4.0,"disk":40,"deprecation":null,
   "locations":[{"name":"nbg1","available":true,"recommended":true}],
   "prices":[{"location":"nbg1","price_monthly":{"net":"3.7900","gross":"4.5101"},"price_hourly":{"net":"0.0060","gross":"0.0071"}}]}
],"meta":{"pagination":{"next_page":null}}}"#;

/// A JSON `GET path` route that answers only requests carrying `Bearer {token}`.
fn route(
    server: &mut mockito::Server,
    path: &str,
    status: usize,
    body: &str,
    token: &str,
) -> mockito::Mock {
    server
        .mock("GET", path)
        .match_query(mockito::Matcher::Any)
        .match_header("authorization", format!("Bearer {token}").as_str())
        .with_status(status)
        .with_header("content-type", "application/json")
        .with_body(body)
}

/// What the page could see, kept for the search.
#[derive(Default)]
struct Seen(Vec<String>);

impl Seen {
    fn reply(&mut self, reply: Result<Value, Value>) -> Value {
        let value = reply.unwrap_or_else(|e| panic!("refused: {e}"));
        self.0.push(value.to_string());
        self.0.push(format!("{value:?}"));
        value
    }

    fn follow(&mut self, rig: &Rig, id: &Value) -> Vec<OpEvent> {
        let events = follow_to_end(rig, OpId(id.as_u64().expect("an op id")));
        for e in &events {
            self.0.push(serde_json::to_string(e).unwrap());
            self.0.push(format!("{e:?}"));
        }
        events
    }
}

fn completed(events: &[OpEvent]) -> Value {
    match events.last() {
        Some(OpEvent::Finished {
            outcome: Outcome::Completed { result },
        }) => result.clone(),
        other => panic!("not completed: {other:?}"),
    }
}

#[test]
fn the_token_crosses_ipc_once_and_comes_back_in_nothing() {
    let (verify, renew, rejected) = ("v".repeat(64), "r".repeat(64), "x".repeat(64));
    let mut server = mockito::Server::new();
    let locations = route(&mut server, "/v1/locations", 200, LOCATIONS_OK, &verify)
        .expect_at_least(2)
        .create();
    let types = route(&mut server, "/v1/server_types", 200, SERVER_TYPES, &verify)
        .expect_at_least(1)
        .create();
    let renewed = route(&mut server, "/v1/locations", 200, LOCATIONS_OK, &renew)
        .expect(1)
        .create();
    let refused = route(&mut server, "/v1/locations", 401, UNAUTHORIZED, &rejected)
        .expect(1)
        .create();
    let rig = rig_with_api(lock_off(), &server.url());
    let mut seen = Seen::default();

    // Verify: the one crossing, as the command's own parameter.
    let id = seen.reply(invoke(
        &rig,
        "op_start_verify_token",
        json!({ "provider": "hetzner-cloud", "token": verify }),
    ));
    let draft = completed(&seen.follow(&rig, &id))["draftId"].clone();
    // The draft store while the token waits in it.
    seen.0.push(format!("{:?}", rig.shell.drafts));
    // The catalogue and the add plan name the draft.
    let id = seen.reply(invoke(
        &rig,
        "op_start_machine_catalogue",
        json!({ "source": { "kind": "draft", "draftId": draft } }),
    ));
    completed(&seen.follow(&rig, &id));
    let view = seen.reply(invoke(
        &rig,
        "op_plan_target_add",
        json!({ "args": {
            "name": "prod", "provider": "hetzner-cloud", "draftId": draft,
            "sshKey": null, "region": "nbg1", "tier": "solo", "serverType": "cx22" } }),
    ));
    rig.shell
        .execute(
            OpId(view["opId"].as_u64().unwrap()),
            Arc::new(Recorder::default()),
        )
        .unwrap();
    completed(&seen.follow(&rig, &view["opId"]));
    // The plan took the draft.
    let again = invoke(
        &rig,
        "op_start_machine_catalogue",
        json!({ "source": { "kind": "draft", "draftId": draft } }),
    );
    assert_eq!(code(&again), Some(errors::DRAFT_NOT_FOUND));
    seen.0.push(format!("{again:?}"));
    // Renew: the new token once, to its own plan; the save pings with it.
    let view = seen.reply(invoke(
        &rig,
        "op_plan_target_renew",
        json!({ "name": "prod", "token": renew }),
    ));
    rig.shell
        .execute(
            OpId(view["opId"].as_u64().unwrap()),
            Arc::new(Recorder::default()),
        )
        .unwrap();
    completed(&seen.follow(&rig, &view["opId"]));
    // A rejected token fails its read, and the error does not carry it.
    let id = seen.reply(invoke(
        &rig,
        "op_start_verify_token",
        json!({ "provider": "hetzner-cloud", "token": rejected }),
    ));
    match seen.follow(&rig, &id).last() {
        Some(OpEvent::Failed { error }) => {
            assert_eq!(
                error.code.as_deref(),
                Some("apprafter::target::token_rejected")
            );
        }
        other => panic!("{other:?}"),
    }
    seen.0.push(format!("{:?}", rig.shell.drafts));
    seen.0.push(format!("{:?}", rig.shell.ops.list()));

    for (i, text) in seen.0.iter().enumerate() {
        for token in [&verify, &renew, &rejected] {
            assert!(
                !text.contains(token.as_str()),
                "item {i} carries a token: {text}"
            );
        }
    }
    locations.assert();
    types.assert();
    renewed.assert();
    refused.assert();
    let credentials = rig
        .shell
        .context
        .store()
        .target_dir("prod")
        .join("credentials.yaml");
    assert!(
        std::fs::read_to_string(credentials)
            .unwrap()
            .contains(renew.as_str()),
        "the renewed token is stored"
    );
}
