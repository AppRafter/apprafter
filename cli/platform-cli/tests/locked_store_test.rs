// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A locked target store through the CLI (D.4a c2). Nothing in this CLI can open a vault yet
//! (d4a-d brings the prompt): a command that needs the token refuses with
//! `apprafter::secrets::store_locked` and takes it from nowhere else — not from `HCLOUD_TOKEN`
//! — while one that needs no secret runs and opens no vault. The store is
//! `apprafter_core::secrets::testing::locked_store`'s, in the spawn point's scratch store.

mod common;

use apprafter_core::secrets::testing::locked_store;
use common::spawn::Spawn;

/// 64 ASCII alphanumerics, never printed by any of these runs.
const TOKEN: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const ENV_TOKEN: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

/// stderr with miette's wrapping undone: its `│` gutter dropped, each run of whitespace one space.
fn squeeze(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .replace('│', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `prod` in a locked store, the CLI's default, with a server recorded, so `target ip` needs its
/// token.
fn locked(sp: &Spawn) {
    let root = sp.store();
    locked_store(&root, &[("prod", Some(TOKEN))]);
    let paths = cli_core::TargetStorePaths::for_root(root);
    cli_core::save_global_config(
        &paths,
        &cli_core::GlobalConfig {
            active_target: "prod".into(),
            version: cli_core::TARGET_STORE_VERSION,
        },
    )
    .unwrap();
    let state = cli_state::StatePaths::for_active_target(&paths, "prod");
    std::fs::create_dir_all(state.state_dir()).unwrap();
    std::fs::write(
        state.state_file(),
        r#"{"hetzner_cloud":{"server_id":7,"server_name":"n"}}"#,
    )
    .unwrap();
}

fn run(sp: &Spawn, args: &[&str]) -> (Option<i32>, String, String) {
    let out = sp.cmd(args).output().expect("run apprafter");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        squeeze(&out.stderr),
    )
}

#[test]
fn a_command_that_needs_the_token_refuses_with_store_locked() {
    let sp = Spawn::new();
    locked(&sp);
    let (code, stdout, stderr) = run(&sp, &["target", "ip"]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("apprafter::secrets::store_locked"),
        "{stderr}"
    );
    assert!(!stdout.contains(TOKEN) && !stderr.contains(TOKEN));
}

/// T35: the variable is refused on a locked store, before anything is asked — never the
/// provider's answer (its base is a closed port here).
#[test]
fn hcloud_token_does_not_open_a_locked_store() {
    let sp = Spawn::new().with_env("HCLOUD_TOKEN", ENV_TOKEN);
    locked(&sp);
    let (code, _, stderr) = run(&sp, &["target", "ip"]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("apprafter::secrets::override_refused") && stderr.contains("HCLOUD_TOKEN"),
        "{stderr}"
    );
}

/// T35: `APPRAFTER_AGE_KEY` is refused where a locked store's cache is read; what needs no
/// secret still runs with either variable set.
#[test]
fn apprafter_age_key_is_refused_where_a_cache_is_read() {
    let sp = Spawn::new();
    let key = sp.home().join("elsewhere").join("age.key");
    let sp = sp.with_env("APPRAFTER_AGE_KEY", key);
    locked(&sp);
    let state = cli_state::StatePaths::for_active_target(
        &cli_core::TargetStorePaths::for_root(sp.store()),
        "prod",
    );
    std::fs::write(
        state.state_file(),
        r#"{"hetzner_cloud":{"server_id":7,"server_name":"n","kubeconfig_age":"-----BEGIN AGE ENCRYPTED FILE-----\nx\n-----END AGE ENCRYPTED FILE-----\n"}}"#,
    )
    .unwrap();
    let (code, _, stderr) = run(&sp, &["kubeconfig"]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("apprafter::secrets::override_refused")
            && stderr.contains("APPRAFTER_AGE_KEY"),
        "{stderr}"
    );
    let sp = sp.with_env("HCLOUD_TOKEN", ENV_TOKEN);
    let (code, _, stderr) = run(&sp, &["target", "list"]);
    assert_eq!(code, Some(0), "{stderr}");
}

/// T7: what needs no secret runs on a locked store and opens no vault — here a directory any
/// read of it would fail on.
#[test]
fn a_command_that_needs_no_secret_runs_and_opens_no_vault() {
    let sp = Spawn::new();
    locked(&sp);
    let vault = sp.store().join("vault");
    std::fs::remove_file(&vault).unwrap();
    std::fs::create_dir(&vault).unwrap();
    for args in [&["target", "list"][..], &["target", "show", "prod"][..]] {
        let (code, stdout, stderr) = run(&sp, args);
        assert_eq!(code, Some(0), "{args:?}: {stderr}");
        assert!(!stdout.contains(TOKEN), "{args:?}");
    }
}
