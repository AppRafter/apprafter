// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The Hetzner token of the legacy commands (`apply`, `destroy`, `import`, `kubeconfig`,
//! `node prep` / `node status`, `target firewall`): the core's `target::hetzner_token`, so the
//! secret store's, for the target the command names — its `--target`, else the CLI default —
//! with the core's codes (D.4a T15). In files `HCLOUD_TOKEN` outranks the stored token (R4); on
//! a locked store the core refuses it before anything is asked (`override_refused`, T35). The
//! target is resolved first, as every core command resolves it: a removed target is
//! `target_not_found` whatever `HCLOUD_TOKEN` says.

use apprafter_core::target::hetzner_token as core_token;
use apprafter_core::{Context, TargetRef};
use cli_core::Result;

use crate::context::{into_cli_error, legacy_context};

/// The token a legacy command acts with, for `--target` (the CLI default when `None`).
pub(crate) fn hetzner_token(target_override: Option<&str>) -> Result<String> {
    token_for(&legacy_context()?, target_override)
}

fn token_for(ctx: &Context, target_override: Option<&str>) -> Result<String> {
    let target = match target_override {
        Some(name) => TargetRef::named(ctx, name),
        None => TargetRef::active(ctx),
    }
    .map_err(into_cli_error)?;
    let token = core_token(ctx, &target).map_err(into_cli_error)?;
    Ok(token.expose().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use apprafter_core::MapEnv;
    use cli_core::target::{
        save_global_config, save_target, GlobalConfig, Target, TargetConfig, TargetCredentials,
        TargetStorePaths,
    };
    use cli_core::CliError;

    /// A store at a fresh root holding `targets` (name, token) and the CLI default `active`,
    /// written as the CLI writes them, and a context over it with no override: the core's
    /// `Context::for_desktop`, its home inside the root (T6). The tests that need the CLI's
    /// `HCLOUD_TOKEN` build theirs from a `MapEnv` over the same root, and G3 lists them.
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

    /// The CLI's context over `dir` with `HCLOUD_TOKEN` set to `token` (empty counts as unset).
    fn with_env_token(dir: &tempfile::TempDir, token: &str) -> Context {
        Context::from_cli_env(
            &MapEnv::new()
                .with("APPRAFTER_CONFIG_DIR", dir.path().to_str().unwrap())
                .with("HCLOUD_TOKEN", token),
        )
        .unwrap()
        .with_home_dir(Some(dir.path().join("home")))
    }

    /// R4 in files: `HCLOUD_TOKEN` outranks the stored token of the target the command names.
    #[test]
    fn hcloud_token_outranks_the_stored_one_of_a_named_target() {
        let (d, _, _) = world(&[("default", Some("store-token-value"))], Some("default"));
        assert_eq!(
            token_for(&with_env_token(&d, "env-token-value"), None).unwrap(),
            "env-token-value"
        );
        assert_eq!(
            token_for(&with_env_token(&d, "env-token-value"), Some("default")).unwrap(),
            "env-token-value"
        );
    }

    #[test]
    fn an_empty_hcloud_token_is_unset() {
        let (d, _, _) = world(&[("default", Some("store-token-value"))], Some("default"));
        assert_eq!(
            token_for(&with_env_token(&d, ""), None).unwrap(),
            "store-token-value"
        );
    }

    /// T15: the target is resolved first, so `HCLOUD_TOKEN` no longer makes a removed or an
    /// unknown target look present (the legacy adapter returned it before any lookup).
    #[test]
    fn hcloud_token_no_longer_passes_an_unknown_default() {
        let (d, _, _) = world(&[], Some("gone"));
        match token_for(&with_env_token(&d, "env-token-value"), None).unwrap_err() {
            CliError::TargetNotFound { name, .. } => assert_eq!(name, "gone"),
            e => panic!("expected TargetNotFound, got {e:?}"),
        }
        match token_for(&with_env_token(&d, "env-token-value"), Some("ghost")).unwrap_err() {
            CliError::TargetNotFound { name, .. } => assert_eq!(name, "ghost"),
            e => panic!("expected TargetNotFound, got {e:?}"),
        }
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

    /// T15: one code per refusal, the core's — no `apprafter::cli::other` prose.
    #[test]
    fn the_refusals_are_the_cores() {
        let (_d, ctx, _) = world(&[], None);
        assert!(matches!(
            token_for(&ctx, None).unwrap_err(),
            CliError::NoActiveTarget
        ));
        let (_d, ctx, _) = world(&[("default", None)], Some("default"));
        match token_for(&ctx, None).unwrap_err() {
            CliError::TokenNotStored { name } => assert_eq!(name, "default"),
            e => panic!("expected TokenNotStored, got {e:?}"),
        }
        let (_d, ctx, _) = world(&[("default", Some("t"))], Some("default"));
        match token_for(&ctx, Some("ghost")).unwrap_err() {
            CliError::TargetNotFound { name, available } => {
                assert_eq!((name.as_str(), available.as_str()), ("ghost", "default"));
            }
            e => panic!("expected TargetNotFound, got {e:?}"),
        }
    }

    /// The token is read from its record alone (the core's rule): a damaged record is that
    /// file's error, never "no token".
    #[test]
    fn a_damaged_record_is_its_files_error() {
        let (_d, ctx, paths) = world(&[("default", Some("t"))], Some("default"));
        let creds = paths.target_credentials_file("default");
        std::fs::write(&creds, "hetzner_token: [").unwrap();
        assert!(matches!(
            token_for(&ctx, None),
            Err(CliError::InvalidTargetConfig { ref path, .. }) if *path == creds
        ));
    }
}
