// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Credential resolution chain (cli-dx-task.md §7).
//!
//! Operational commands (`apprafter apply` / `destroy` / `import`
//! / ...) need a Hetzner Cloud API token + (sometimes) an SSH
//! public key. There are three places those values can come from,
//! in priority order:
//!
//!   1. **CLI flag** — `--token <X>` / future `--ssh-key <path>`
//!      flags. Highest priority: an operator typing the flag is
//!      explicit about what they want for this one invocation.
//!   2. **Env var** — `HCLOUD_TOKEN` / `APPRAFTER_SSH_PUBLIC_KEY`
//!      (legacy). The pre-target-store workflow; preserved for CI
//!      pipelines that already export these.
//!   3. **Active target's credentials.yaml** — the post-target-
//!      store path. Picks up the value the wizard or
//!      `apprafter target add` wrote into
//!      `~/.config/apprafter/targets/<active>/credentials.yaml`.
//!      Honours `--target <name>` override.
//!
//! Resolution failure is a typed `CliError::Other` with a hint
//! listing every path the user could take to fix it — no silent
//! fallback to a placeholder. Future Track A.10 swaps the
//! free-form messages for miette-shaped diagnostics; the
//! resolver signatures stay the same.

use std::path::Path;

use crate::target::{load_global_config, load_target, TargetStorePaths};
use crate::{CliError, Result};

/// Env-var name clap uses for the Hetzner Cloud API token on the
/// `target add --token` flag and (historically) on operational
/// commands. Centralised so the resolver picks up the same
/// string as the clap `#[arg(env)]` annotations.
pub const HCLOUD_TOKEN_ENV: &str = "HCLOUD_TOKEN";

/// Env-var name carrying an SSH public key **body** (not a path)
/// inline. Pre-target-store CI scripts export the literal `ssh-
/// ed25519 AAAA…` string under this name.
pub const SSH_PUBLIC_KEY_ENV: &str = "APPRAFTER_SSH_PUBLIC_KEY";

/// Resolve the Hetzner Cloud API token through the §7 chain.
///
/// `cli_flag`: highest-priority override (e.g., future `--token`
/// flag). Pass `None` when the command doesn't expose such a
/// flag yet.
///
/// `paths`: target-store locator. Production callers build it
/// from `default_config_root()?`; tests redirect via
/// `APPRAFTER_CONFIG_DIR`.
///
/// `target_override`: per-invocation `--target <name>` override.
/// `None` falls back to the active target.
///
/// Returns the resolved token string on success. On failure the
/// `CliError::Other` payload enumerates every path the user can
/// take next (flag / env / `apprafter target add`).
pub fn resolve_hetzner_token(
    cli_flag: Option<&str>,
    paths: &TargetStorePaths,
    target_override: Option<&str>,
) -> Result<String> {
    if let Some(t) = cli_flag {
        if !t.is_empty() {
            return Ok(t.to_string());
        }
    }
    if let Ok(t) = std::env::var(HCLOUD_TOKEN_ENV) {
        if !t.is_empty() {
            return Ok(t);
        }
    }
    let Some(name) = resolve_target_name(paths, target_override)? else {
        return Err(CliError::Other(format!(
            "no Hetzner Cloud token available. Either pass `--token <X>`, set `{HCLOUD_TOKEN_ENV}` env var, or run `apprafter target add` to configure an active target."
        )));
    };
    match load_target(paths, &name) {
        Ok(t) => t.credentials.hetzner_token.ok_or_else(|| {
            CliError::Other(format!(
                "target `{name}` has no Hetzner Cloud token stored. Run `apprafter target add {name} --renew --token <X>` to add one, or pass `--token`/`{HCLOUD_TOKEN_ENV}` for this invocation."
            ))
        }),
        Err(CliError::TargetNotFound { available, .. }) => Err(CliError::Other(format!(
            "target `{name}` not found (available: {available}). Pass `--target <name>` with a configured name, or `apprafter target use <name>` to switch the active pointer."
        ))),
        Err(e) => Err(e),
    }
}

/// Resolve the SSH public key BODY (the `ssh-ed25519 AAAA… me@`
/// string Hetzner expects) through the §7 chain.
///
/// Returns `Ok(None)` when no key is configured anywhere — apply
/// is allowed to proceed without an SSH key today (the server
/// still boots; Hetzner returns a root password). Caller may
/// surface that as a warning.
///
/// On found-but-unreadable target path, returns a typed error
/// rather than silently dropping back to the no-key path.
pub fn resolve_hetzner_ssh_public_key(
    paths: &TargetStorePaths,
    target_override: Option<&str>,
) -> Result<Option<String>> {
    // 1. Env var carrying the body inline. Same convention as
    //    the pre-target-store workflow — single source of truth
    //    for both old + new CI scripts.
    if let Ok(body) = std::env::var(SSH_PUBLIC_KEY_ENV) {
        if !body.is_empty() {
            return Ok(Some(body));
        }
    }
    // 2. Target store path. Optional — apply works without one.
    let Some(name) = resolve_target_name(paths, target_override)? else {
        return Ok(None);
    };
    let target = match load_target(paths, &name) {
        Ok(t) => t,
        // Missing target — caller already gets a clearer error
        // from `resolve_hetzner_token` when it asks for the
        // token first. Here we tolerate to keep the ssh-key
        // path strictly optional.
        Err(CliError::TargetNotFound { .. }) => return Ok(None),
        Err(e) => return Err(e),
    };
    let Some(path) = target.config.ssh_key_path.as_ref() else {
        return Ok(None);
    };
    read_ssh_public_key_body(path).map(Some)
}

/// Pure helper exposing the file read so callers wanting to
/// validate the SSH key path without the full resolution chain
/// (e.g. `apprafter doctor`) can reuse the same parser.
pub fn read_ssh_public_key_body(path: &Path) -> Result<String> {
    let body = std::fs::read_to_string(path).map_err(|e| {
        CliError::Other(format!(
            "cannot read SSH public key at `{}`: {e}. Update the target's `ssh_key_path` or set `{SSH_PUBLIC_KEY_ENV}` to the key body inline.",
            path.display()
        ))
    })?;
    let trimmed = body.trim().to_string();
    if trimmed.is_empty() {
        return Err(CliError::Other(format!(
            "SSH public key at `{}` is empty",
            path.display()
        )));
    }
    Ok(trimmed)
}

/// Internal: which target should we consult? `--target` wins;
/// then the active target from `GlobalConfig`; otherwise `None`
/// to let the caller decide if a missing target is an error or
/// just a fallback miss.
fn resolve_target_name(
    paths: &TargetStorePaths,
    target_override: Option<&str>,
) -> Result<Option<String>> {
    if let Some(n) = target_override {
        return Ok(Some(n.to_string()));
    }
    Ok(load_global_config(paths)?
        .map(|g| g.active_target)
        .filter(|s| !s.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::{
        save_global_config, save_target, GlobalConfig, Target, TargetConfig, TargetCredentials,
        CONFIG_DIR_ENV,
    };
    use tempfile::tempdir;

    fn make_paths() -> (tempfile::TempDir, TargetStorePaths) {
        let dir = tempdir().unwrap();
        let paths = TargetStorePaths::for_root(dir.path().to_path_buf());
        (dir, paths)
    }

    fn seed_target(paths: &TargetStorePaths, name: &str, token: Option<&str>) {
        save_target(
            paths,
            &Target {
                name: name.into(),
                config: TargetConfig {
                    provider: "hetzner-cloud".into(),
                    ..Default::default()
                },
                credentials: TargetCredentials {
                    hetzner_token: token.map(String::from),
                },
            },
        )
        .unwrap();
        save_global_config(
            paths,
            &GlobalConfig {
                active_target: name.into(),
                version: 1,
            },
        )
        .unwrap();
    }

    fn with_clean_env<F: FnOnce()>(body: F) {
        // Serialise with the crate-wide test mutex so other
        // modules (`target::tests`) that flip the same env vars
        // can't race us. PoisonError from a previous panic
        // doesn't matter — unwrap_or_else gets the inner guard.
        let _guard = crate::TEST_ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior_token = std::env::var(HCLOUD_TOKEN_ENV).ok();
        let prior_key = std::env::var(SSH_PUBLIC_KEY_ENV).ok();
        let prior_root = std::env::var(CONFIG_DIR_ENV).ok();
        std::env::remove_var(HCLOUD_TOKEN_ENV);
        std::env::remove_var(SSH_PUBLIC_KEY_ENV);
        // CONFIG_DIR_ENV is set by individual tests when needed.
        std::env::remove_var(CONFIG_DIR_ENV);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
        if let Some(v) = prior_token {
            std::env::set_var(HCLOUD_TOKEN_ENV, v);
        }
        if let Some(v) = prior_key {
            std::env::set_var(SSH_PUBLIC_KEY_ENV, v);
        }
        if let Some(v) = prior_root {
            std::env::set_var(CONFIG_DIR_ENV, v);
        }
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    #[test]
    fn resolve_token_cli_flag_wins_over_env_and_store() {
        with_clean_env(|| {
            let (_dir, paths) = make_paths();
            seed_target(&paths, "default", Some("store-token-value"));
            std::env::set_var(HCLOUD_TOKEN_ENV, "env-token-value");
            let resolved = resolve_hetzner_token(Some("flag-token-value"), &paths, None).unwrap();
            assert_eq!(resolved, "flag-token-value");
            std::env::remove_var(HCLOUD_TOKEN_ENV);
        });
    }

    #[test]
    fn resolve_token_env_wins_over_target_store_when_no_flag() {
        with_clean_env(|| {
            let (_dir, paths) = make_paths();
            seed_target(&paths, "default", Some("store-token-value"));
            std::env::set_var(HCLOUD_TOKEN_ENV, "env-token-value");
            let resolved = resolve_hetzner_token(None, &paths, None).unwrap();
            assert_eq!(resolved, "env-token-value");
            std::env::remove_var(HCLOUD_TOKEN_ENV);
        });
    }

    #[test]
    fn resolve_token_falls_back_to_target_store_when_flag_and_env_absent() {
        with_clean_env(|| {
            let (_dir, paths) = make_paths();
            seed_target(&paths, "default", Some("store-token-value"));
            let resolved = resolve_hetzner_token(None, &paths, None).unwrap();
            assert_eq!(resolved, "store-token-value");
        });
    }

    #[test]
    fn resolve_token_target_override_picks_named_target_not_active() {
        with_clean_env(|| {
            let (_dir, paths) = make_paths();
            // Save TWO targets; active is `primary`, but the
            // override asks for `secondary`.
            save_target(
                &paths,
                &Target {
                    name: "primary".into(),
                    config: TargetConfig {
                        provider: "hetzner-cloud".into(),
                        ..Default::default()
                    },
                    credentials: TargetCredentials {
                        hetzner_token: Some("primary-token".into()),
                    },
                },
            )
            .unwrap();
            save_target(
                &paths,
                &Target {
                    name: "secondary".into(),
                    config: TargetConfig {
                        provider: "hetzner-cloud".into(),
                        ..Default::default()
                    },
                    credentials: TargetCredentials {
                        hetzner_token: Some("secondary-token".into()),
                    },
                },
            )
            .unwrap();
            save_global_config(
                &paths,
                &GlobalConfig {
                    active_target: "primary".into(),
                    version: 1,
                },
            )
            .unwrap();

            let resolved = resolve_hetzner_token(None, &paths, Some("secondary")).unwrap();
            assert_eq!(resolved, "secondary-token");
        });
    }

    #[test]
    fn resolve_token_errors_with_three_paths_hint_when_nothing_configured() {
        with_clean_env(|| {
            let (_dir, paths) = make_paths();
            // No target, no env, no flag.
            let err = resolve_hetzner_token(None, &paths, None).expect_err("must error");
            match err {
                CliError::Other(msg) => {
                    assert!(msg.contains("--token"), "{msg}");
                    assert!(msg.contains("HCLOUD_TOKEN"), "{msg}");
                    assert!(msg.contains("apprafter target add"), "{msg}");
                }
                other => panic!("expected Other, got {other:?}"),
            }
        });
    }

    #[test]
    fn resolve_token_errors_when_target_exists_but_no_token_stored() {
        with_clean_env(|| {
            let (_dir, paths) = make_paths();
            // Target with no token (e.g., partially-configured
            // dotfiles drop).
            seed_target(&paths, "default", None);
            let err = resolve_hetzner_token(None, &paths, None).expect_err("must error");
            match err {
                CliError::Other(msg) => {
                    assert!(msg.contains("has no Hetzner Cloud token stored"), "{msg}");
                    assert!(msg.contains("--renew"), "{msg}");
                }
                other => panic!("expected Other, got {other:?}"),
            }
        });
    }

    #[test]
    fn resolve_token_with_override_for_missing_target_surfaces_available_hint() {
        with_clean_env(|| {
            let (_dir, paths) = make_paths();
            seed_target(&paths, "default", Some("token"));
            let err =
                resolve_hetzner_token(None, &paths, Some("ghost")).expect_err("missing target");
            match err {
                CliError::Other(msg) => {
                    assert!(msg.contains("not found"), "{msg}");
                    assert!(msg.contains("default"), "{msg}");
                    assert!(msg.contains("--target"), "{msg}");
                }
                other => panic!("expected Other, got {other:?}"),
            }
        });
    }

    #[test]
    fn resolve_ssh_public_key_env_wins_over_target_path() {
        with_clean_env(|| {
            let (_dir, paths) = make_paths();
            // Write a real key file but ALSO set the env var.
            let key_dir = tempdir().unwrap();
            let key_path = key_dir.path().join("file.pub");
            std::fs::write(&key_path, "ssh-ed25519 AAAA-from-file me@file").unwrap();
            save_target(
                &paths,
                &Target {
                    name: "default".into(),
                    config: TargetConfig {
                        provider: "hetzner-cloud".into(),
                        ssh_key_path: Some(key_path.clone()),
                        ..Default::default()
                    },
                    credentials: TargetCredentials::default(),
                },
            )
            .unwrap();
            save_global_config(
                &paths,
                &GlobalConfig {
                    active_target: "default".into(),
                    version: 1,
                },
            )
            .unwrap();
            std::env::set_var(SSH_PUBLIC_KEY_ENV, "ssh-ed25519 AAAA-from-env me@env");

            let resolved = resolve_hetzner_ssh_public_key(&paths, None).unwrap();
            assert_eq!(resolved.unwrap(), "ssh-ed25519 AAAA-from-env me@env");
            std::env::remove_var(SSH_PUBLIC_KEY_ENV);
        });
    }

    #[test]
    fn resolve_ssh_public_key_reads_target_path_when_env_absent() {
        with_clean_env(|| {
            let (_dir, paths) = make_paths();
            let key_dir = tempdir().unwrap();
            let key_path = key_dir.path().join("file.pub");
            std::fs::write(&key_path, "ssh-ed25519 AAAA-from-file me@file\n").unwrap();
            save_target(
                &paths,
                &Target {
                    name: "default".into(),
                    config: TargetConfig {
                        provider: "hetzner-cloud".into(),
                        ssh_key_path: Some(key_path.clone()),
                        ..Default::default()
                    },
                    credentials: TargetCredentials::default(),
                },
            )
            .unwrap();
            save_global_config(
                &paths,
                &GlobalConfig {
                    active_target: "default".into(),
                    version: 1,
                },
            )
            .unwrap();

            let resolved = resolve_hetzner_ssh_public_key(&paths, None).unwrap();
            // Trimmed — no trailing newline.
            assert_eq!(resolved.unwrap(), "ssh-ed25519 AAAA-from-file me@file");
        });
    }

    #[test]
    fn resolve_ssh_public_key_returns_none_when_nothing_configured() {
        with_clean_env(|| {
            let (_dir, paths) = make_paths();
            // No target at all → no SSH key.
            assert!(resolve_hetzner_ssh_public_key(&paths, None)
                .unwrap()
                .is_none());
        });
    }

    #[test]
    fn resolve_ssh_public_key_errors_loudly_on_unreadable_path() {
        with_clean_env(|| {
            let (_dir, paths) = make_paths();
            // Target points at a nonexistent path.
            save_target(
                &paths,
                &Target {
                    name: "default".into(),
                    config: TargetConfig {
                        provider: "hetzner-cloud".into(),
                        ssh_key_path: Some("/this/does/not/exist.pub".into()),
                        ..Default::default()
                    },
                    credentials: TargetCredentials::default(),
                },
            )
            .unwrap();
            save_global_config(
                &paths,
                &GlobalConfig {
                    active_target: "default".into(),
                    version: 1,
                },
            )
            .unwrap();

            let err = resolve_hetzner_ssh_public_key(&paths, None).expect_err("path missing");
            match err {
                CliError::Other(msg) => {
                    assert!(msg.contains("cannot read SSH public key"), "{msg}");
                    assert!(msg.contains(SSH_PUBLIC_KEY_ENV), "{msg}");
                }
                other => panic!("expected Other, got {other:?}"),
            }
        });
    }
}
