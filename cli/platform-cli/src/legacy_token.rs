// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The Hetzner token of the legacy commands (`apply`, `destroy`, `import`, `kubeconfig`,
//! `node prep` / `node status`, `target firewall`), read through the core's
//! `apprafter_core::target::hetzner_token` — so through the secret store — in today's order and
//! with today's words: `HCLOUD_TOKEN` before any target is looked at; the target's
//! `config.yaml` read first, as `cli_core::load_target` read it; the refusals worded as
//! `cli_core::resolve_hetzner_token` worded them (D.4a T15). Temporary: d4a-d deletes this file
//! and moves those goldens to the core's codes in one deliberate commit.

use apprafter_core::target::hetzner_token as core_token;
use apprafter_core::{Context, CoreError, TargetRef};
use cli_core::{CliError, Result};

use crate::context::{into_cli_error, legacy_context};

/// The token a legacy command acts with, for `--target` (the active target when `None`).
pub(crate) fn hetzner_token(target_override: Option<&str>) -> Result<String> {
    token_for(&legacy_context()?, target_override)
}

fn token_for(ctx: &Context, target_override: Option<&str>) -> Result<String> {
    if let Some(token) = &ctx.overrides().hetzner_token {
        return Ok(token.expose().to_string());
    }
    let store = ctx.store();
    let Some(name) = cli_core::resolve_active_target_name(&store, target_override)? else {
        return Err(CliError::Other(
            "no Hetzner Cloud token available. Either pass `--token <X>`, set `HCLOUD_TOKEN` env var, or run `apprafter target add` to configure an active target.".to_string(),
        ));
    };
    cli_core::target::load_target_config(&store, &name)?;
    let target = TargetRef::named(ctx, &name).map_err(into_cli_error)?;
    match core_token(ctx, &target) {
        Ok(token) => Ok(token.expose().to_string()),
        Err(CoreError::TokenNotStored { name }) => Err(CliError::Other(format!(
            "target `{name}` has no Hetzner Cloud token stored. Run `apprafter target add {name} --renew --token <X>` to add one, or pass `--token`/`HCLOUD_TOKEN` for this invocation."
        ))),
        Err(e) => Err(into_cli_error(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apprafter_core::MapEnv;
    use cli_core::target::{
        save_global_config, save_target, GlobalConfig, Target, TargetConfig, TargetCredentials,
        TargetStorePaths,
    };

    /// A store at a fresh root holding `targets` (name, token) and the CLI default `active`,
    /// written as the CLI writes them, and a context over it with no override: the core's
    /// `Context::for_desktop`, its home inside the root, so no path of it names the account's
    /// own files and G3 has nothing to list (T6). The two tests that need the CLI's
    /// `HCLOUD_TOKEN` override build theirs inline, and G3 lists them by name.
    fn world(
        targets: &[(&str, Option<&str>)],
        active: Option<&str>,
    ) -> (tempfile::TempDir, Context, TargetStorePaths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = TargetStorePaths::for_root(dir.path().to_path_buf());
        for (name, token) in targets {
            save_target(
                &paths,
                &Target {
                    name: name.to_string(),
                    config: TargetConfig {
                        provider: "hetzner-cloud".into(),
                        ..Default::default()
                    },
                    credentials: TargetCredentials {
                        hetzner_token: token.map(str::to_string),
                    },
                },
            )
            .unwrap();
        }
        if let Some(a) = active {
            save_global_config(
                &paths,
                &GlobalConfig {
                    active_target: a.into(),
                    version: 1,
                },
            )
            .unwrap();
        }
        let ctx = Context::for_desktop(dir.path().to_path_buf(), "http://127.0.0.1:1")
            .with_home_dir(Some(dir.path().join("home")));
        (dir, ctx, paths)
    }

    fn other(e: CliError) -> String {
        match e {
            CliError::Other(m) => m,
            e => panic!("expected Other, got {e:?}"),
        }
    }

    /// Today's order: `HCLOUD_TOKEN` is returned before any target is looked at — a dangling
    /// default or an unknown `--target` included.
    #[test]
    fn hcloud_token_wins_before_any_target_is_looked_at() {
        // The override needs the CLI's builder: read from a MapEnv over the tempdir store,
        // never this process's environment, its home moved into the tempdir (G3 lists this
        // test; it opens nothing of the account's).
        let cli = |dir: &tempfile::TempDir| {
            Context::from_cli_env(
                &MapEnv::new()
                    .with("APPRAFTER_CONFIG_DIR", dir.path().to_str().unwrap())
                    .with("HCLOUD_TOKEN", "env-token-value"),
            )
            .unwrap()
            .with_home_dir(Some(dir.path().join("home")))
        };
        let (d, _, _) = world(&[("default", Some("store-token-value"))], Some("default"));
        assert_eq!(token_for(&cli(&d), None).unwrap(), "env-token-value");
        let (d, _, _) = world(&[], Some("gone"));
        assert_eq!(token_for(&cli(&d), None).unwrap(), "env-token-value");
        assert_eq!(
            token_for(&cli(&d), Some("ghost")).unwrap(),
            "env-token-value"
        );
    }

    #[test]
    fn an_empty_hcloud_token_is_unset() {
        let (d, _, _) = world(&[("default", Some("store-token-value"))], Some("default"));
        // The CLI's builder reads the empty `HCLOUD_TOKEN`: a MapEnv over the tempdir store, its
        // home moved into the tempdir (G3 lists this test).
        let ctx = Context::from_cli_env(
            &MapEnv::new()
                .with("APPRAFTER_CONFIG_DIR", d.path().to_str().unwrap())
                .with("HCLOUD_TOKEN", ""),
        )
        .unwrap()
        .with_home_dir(Some(d.path().join("home")));
        assert_eq!(token_for(&ctx, None).unwrap(), "store-token-value");
    }

    #[test]
    fn the_stored_token_of_the_named_target_else_the_active_one() {
        let (_d, ctx, _) = world(
            &[
                ("primary", Some("primary-token")),
                ("secondary", Some("secondary-token")),
            ],
            Some("primary"),
        );
        assert_eq!(token_for(&ctx, None).unwrap(), "primary-token");
        assert_eq!(
            token_for(&ctx, Some("secondary")).unwrap(),
            "secondary-token"
        );
    }

    /// T15: the refusals keep `resolve_hetzner_token`'s words until d4a-d moves their goldens.
    #[test]
    fn the_refusals_keep_todays_words() {
        let (_d, ctx, _) = world(&[], None);
        assert_eq!(
            other(token_for(&ctx, None).unwrap_err()),
            "no Hetzner Cloud token available. Either pass `--token <X>`, set `HCLOUD_TOKEN` env var, or run `apprafter target add` to configure an active target."
        );
        let (_d, ctx, _) = world(&[("default", None)], Some("default"));
        assert_eq!(
            other(token_for(&ctx, None).unwrap_err()),
            "target `default` has no Hetzner Cloud token stored. Run `apprafter target add default --renew --token <X>` to add one, or pass `--token`/`HCLOUD_TOKEN` for this invocation."
        );
        let (_d, ctx, _) = world(&[("default", Some("t"))], Some("default"));
        match token_for(&ctx, Some("ghost")).unwrap_err() {
            CliError::TargetNotFound { name, available } => {
                assert_eq!(name, "ghost");
                assert_eq!(available, "default");
            }
            e => panic!("expected TargetNotFound, got {e:?}"),
        }
    }

    /// Today's `load_target` read the config first, then the credentials: each file's own error.
    #[test]
    fn a_target_is_read_as_load_target_read_it() {
        let (_d, ctx, paths) = world(&[("default", Some("t"))], Some("default"));
        let config = paths.target_config_file("default");
        std::fs::write(&config, "provider: [").unwrap();
        assert!(matches!(
            token_for(&ctx, None),
            Err(CliError::InvalidTargetConfig { ref path, .. }) if *path == config
        ));
        std::fs::remove_file(&config).unwrap();
        assert!(matches!(
            token_for(&ctx, None),
            Err(CliError::TargetNotFound { ref name, .. }) if name == "default"
        ));
        std::fs::write(&config, "provider: hetzner-cloud\n").unwrap();
        let creds = paths.target_credentials_file("default");
        std::fs::write(&creds, "hetzner_token: [").unwrap();
        assert!(matches!(
            token_for(&ctx, None),
            Err(CliError::InvalidTargetConfig { ref path, .. }) if *path == creds
        ));
    }
}
