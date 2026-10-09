// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Fixtures for the target family's tests: a scratch store, seeded state, Hetzner mocks.

use cli_core::target::{
    GlobalConfig, Target, TargetConfig, TargetCredentials, TARGET_STORE_VERSION,
};

use crate::Context;

pub(crate) const TOKEN_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// Targets `names` (provider hetzner-cloud, region nbg1, tier solo, TOKEN_A), pointer `active`.
pub(crate) fn store(names: &[&str], active: Option<&str>) -> (tempfile::TempDir, Context) {
    store_at(names, active, "http://127.0.0.1:1")
}

/// [`store`] with the Hetzner API at `api`.
pub(crate) fn store_at(
    names: &[&str],
    active: Option<&str>,
    api: &str,
) -> (tempfile::TempDir, Context) {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Context::for_desktop(dir.path().join("store"), api)
        .with_home_dir(Some(dir.path().join("home")));
    for n in names {
        cli_core::save_target(
            &ctx.store(),
            &Target {
                name: n.to_string(),
                config: TargetConfig {
                    provider: "hetzner-cloud".into(),
                    region: Some("nbg1".into()),
                    default_tier: Some("solo".into()),
                    ..Default::default()
                },
                credentials: TargetCredentials {
                    hetzner_token: Some(TOKEN_A.into()),
                },
            },
        )
        .unwrap();
    }
    if let Some(a) = active {
        cli_core::save_global_config(
            &ctx.store(),
            &GlobalConfig {
                active_target: a.into(),
                version: TARGET_STORE_VERSION,
            },
        )
        .unwrap();
    }
    (dir, ctx)
}

/// `state/<name>` records server `server` (id `id`, type `sku`).
pub(crate) fn seed_server(ctx: &Context, name: &str, id: u64, server: &str, sku: Option<&str>) {
    let sku = sku.map_or("null".to_string(), |s| format!("\"{s}\""));
    seed_state_raw(
        ctx,
        name,
        &format!(
            r#"{{"hetzner_cloud":{{"server_id":{id},"server_name":"{server}","server_type":{sku}}}}}"#
        ),
    );
}

/// `state/<name>/.apprafter/state.json` is `body`, verbatim.
pub(crate) fn seed_state_raw(ctx: &Context, name: &str, body: &str) {
    let p = cli_state::StatePaths::for_active_target(&ctx.store(), name);
    std::fs::create_dir_all(p.state_dir()).unwrap();
    std::fs::write(p.state_file(), body).unwrap();
}

/// Load target `name`, change it with `f`, save it back.
pub(crate) fn edit(ctx: &Context, name: &str, f: impl FnOnce(&mut Target)) {
    let mut t = cli_core::load_target(&ctx.store(), name).unwrap();
    f(&mut t);
    cli_core::save_target(&ctx.store(), &t).unwrap();
}

pub(crate) const TOKEN_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// `GET path` (any query) answering `status` / `body` only to `Bearer token`.
pub(crate) fn route(
    s: &mut mockito::Server,
    path: &str,
    status: usize,
    body: &str,
    token: &str,
) -> mockito::Mock {
    s.mock("GET", path)
        .match_query(mockito::Matcher::Any)
        .match_header("authorization", format!("Bearer {token}").as_str())
        .with_status(status)
        .with_header("content-type", "application/json")
        .with_body(body)
}

/// [`route`] answering 200 / `body`, cancelling `cancel` as it answers: a cancel that lands
/// while the request is in flight, which the provider calls do not see (they check before they
/// send), so only a check after them can.
pub(crate) fn route_cancelling(
    s: &mut mockito::Server,
    path: &str,
    body: &'static str,
    token: &str,
    cancel: &crate::CancellationToken,
) -> mockito::Mock {
    let tripped = cancel.clone();
    route(s, path, 200, body, token).with_body_from_request(move |_| {
        tripped.cancel();
        body.as_bytes().to_vec()
    })
}

/// A token that is already cancelled.
pub(crate) fn cancelled_token() -> crate::CancellationToken {
    let c = crate::CancellationToken::new();
    c.cancel();
    c
}

pub(crate) const LOCATIONS: &str = r#"{"locations":[
 {"id":2,"name":"nbg1","description":"Nuremberg DC Park 1","country":"DE","city":"Nuremberg","network_zone":"eu-central"},
 {"id":1,"name":"fsn1","description":"Falkenstein DC Park 1","country":"DE","city":"Falkenstein","network_zone":"eu-central"}]}"#;

/// cx22 (nbg1, recommended), cx32 (nbg1 + fsn1), cx11 retired in nbg1 (unavailable_after in 2020).
pub(crate) const SERVER_TYPES: &str = r#"{"server_types":[
 {"id":104,"name":"cx22","architecture":"x86","cpu_type":"shared","cores":2,"memory":4.0,"disk":40,"deprecation":null,
  "locations":[{"name":"nbg1","available":true,"recommended":true}],
  "prices":[{"location":"nbg1","price_monthly":{"net":"3.7900","gross":"4.5101"},"price_hourly":{"net":"0.0060","gross":"0.0071"}}]},
 {"id":105,"name":"cx32","architecture":"x86","cpu_type":"shared","cores":4,"memory":8.0,"disk":80,"deprecation":null,
  "locations":[{"name":"nbg1","available":true,"recommended":false},{"name":"fsn1","available":true,"recommended":false}],"prices":[]},
 {"id":1,"name":"cx11","architecture":"x86","cpu_type":"shared","cores":1,"memory":2.0,"disk":20,"deprecation":null,
  "locations":[{"name":"nbg1","available":false,"recommended":false,"deprecation":{"announced":"2019-01-01T00:00:00+00:00","unavailable_after":"2020-01-01T00:00:00+00:00"}}],"prices":[]}
],"meta":{"pagination":{"next_page":null}}}"#;

/// Save a fresh hetzner-cloud target `name` (no region, tier or token) — another process's add.
pub(crate) fn edit_new(ctx: &Context, name: &str) {
    cli_core::save_target(
        &ctx.store(),
        &Target {
            name: name.to_string(),
            config: TargetConfig {
                provider: "hetzner-cloud".into(),
                ..Default::default()
            },
            credentials: TargetCredentials::default(),
        },
    )
    .unwrap();
}
