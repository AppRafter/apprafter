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
