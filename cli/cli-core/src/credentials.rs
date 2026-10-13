// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The SSH public key a provisioning command sends (cli-dx-task.md §7):
//! `APPRAFTER_SSH_PUBLIC_KEY` (the key body inline), else the key file the
//! target's `config.yaml` names (`--target`, else the active target).
//!
//! The Hetzner token is not resolved here any more: every command reads it
//! through the shared core (`apprafter_core::target::hetzner_token`, through
//! its secret store; D.4a b). `HCLOUD_TOKEN_ENV` stays the one name clap's
//! `#[arg(env)]` annotations and the core's `Context` read.

use std::path::Path;

use crate::target::{load_global_config, load_target_config, TargetStorePaths};
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
            // GOTCHA-149: whatever is returned here is sent to the provider as is.
            crate::ssh_key::parse_public_key(&body).map_err(|e| {
                e.refusal(
                    format!("`{SSH_PUBLIC_KEY_ENV}`"),
                    crate::ssh_key::KeySource::Env,
                )
            })?;
            return Ok(Some(body));
        }
    }
    // 2. Target store path. Optional — apply works without one. The config alone: the
    //    credentials file is the shared core's secret store's to read (D.4a b).
    let Some(name) = resolve_target_name(paths, target_override)? else {
        return Ok(None);
    };
    let config = match load_target_config(paths, &name) {
        Ok(c) => c,
        // Missing target — the caller already gets a clearer error from
        // the token read when it asks for the token first. Here we tolerate
        // it to keep the ssh-key path strictly optional.
        Err(CliError::TargetNotFound { .. }) => return Ok(None),
        Err(e) => return Err(e),
    };
    let Some(path) = config.ssh_key_path.as_ref() else {
        return Ok(None);
    };
    read_ssh_public_key_body(path).map(Some)
}

/// Pure helper exposing the file read so callers wanting to
/// validate the SSH key path without the full resolution chain
/// (e.g. `apprafter doctor`) can reuse the same parser. The body is
/// what the provider is sent, so it must be one OpenSSH public key
/// line: a private key or anything else is refused before any upload
/// (`CliError::SshKeyNotPublic`, GOTCHA-149), and never quoted.
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
    crate::ssh_key::parse_public_key(&trimmed).map_err(|e| {
        e.refusal(
            crate::ssh_key::file_origin(path),
            crate::ssh_key::KeySource::TargetFile,
        )
    })?;
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
    fn resolve_ssh_public_key_env_wins_over_target_path() {
        with_clean_env(|| {
            let (_dir, paths) = make_paths();
            // Write a real key file but ALSO set the env var.
            let key_dir = tempdir().unwrap();
            let key_path = key_dir.path().join("file.pub");
            std::fs::write(&key_path, "ssh-ed25519 AAAAZmlsZQ== me@file").unwrap();
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
            std::env::set_var(SSH_PUBLIC_KEY_ENV, "ssh-ed25519 AAAAZW52 me@env");

            let resolved = resolve_hetzner_ssh_public_key(&paths, None).unwrap();
            assert_eq!(resolved.unwrap(), "ssh-ed25519 AAAAZW52 me@env");
            std::env::remove_var(SSH_PUBLIC_KEY_ENV);
        });
    }

    #[test]
    fn resolve_ssh_public_key_reads_target_path_when_env_absent() {
        with_clean_env(|| {
            let (_dir, paths) = make_paths();
            let key_dir = tempdir().unwrap();
            let key_path = key_dir.path().join("file.pub");
            std::fs::write(&key_path, "ssh-ed25519 AAAAZmlsZQ== me@file\n").unwrap();
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
            assert_eq!(resolved.unwrap(), "ssh-ed25519 AAAAZmlsZQ== me@file");
        });
    }

    /// GOTCHA-149: what `apply` sends the provider as the target's SSH key is checked first. A
    /// private key (the file next to the `.pub`) or anything that is not one OpenSSH public key
    /// line is refused, by path, and its text is never quoted.
    #[test]
    fn the_body_sent_to_the_provider_must_be_a_public_key() {
        let dir = tempdir().unwrap();
        let private = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjE=\n-----END OPENSSH PRIVATE KEY-----\n";
        for (body, private_key) in [(private, true), ("not-a-key\n", false)] {
            let path = dir.path().join("id_ed25519");
            std::fs::write(&path, body).unwrap();
            let err = read_ssh_public_key_body(&path).expect_err(body);
            assert!(!err.to_string().contains(body.trim()), "{err}");
            match err {
                CliError::SshKeyNotPublic {
                    origin,
                    private_key: p,
                    from,
                } => {
                    assert_eq!(origin, format!("SSH key `{}`", path.display()));
                    assert_eq!(p, private_key, "{body}");
                    // The stored path of an existing target (apply, doctor).
                    assert_eq!(from, crate::ssh_key::KeySource::TargetFile);
                }
                other => panic!("{other:?}"),
            }
        }
        let path = dir.path().join("id_ed25519.pub");
        std::fs::write(&path, "ssh-ed25519 AAAA me@host\n").unwrap();
        assert_eq!(
            read_ssh_public_key_body(&path).unwrap(),
            "ssh-ed25519 AAAA me@host"
        );
    }

    /// The same for a body carried inline in `APPRAFTER_SSH_PUBLIC_KEY`.
    #[test]
    fn an_inline_key_body_must_be_a_public_key_too() {
        with_clean_env(|| {
            let (_dir, paths) = make_paths();
            std::env::set_var(
                SSH_PUBLIC_KEY_ENV,
                "-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----",
            );
            match resolve_hetzner_ssh_public_key(&paths, None).expect_err("a private key") {
                CliError::SshKeyNotPublic {
                    origin,
                    private_key,
                    from,
                } => {
                    assert_eq!(origin, format!("`{SSH_PUBLIC_KEY_ENV}`"));
                    assert!(private_key);
                    assert_eq!(from, crate::ssh_key::KeySource::Env);
                }
                other => panic!("{other:?}"),
            }
            std::env::remove_var(SSH_PUBLIC_KEY_ENV);
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

    /// D.4a b: the key path comes from the target's config alone — a credentials file that
    /// cannot be read (the secret store's to read) does not stop it.
    #[test]
    fn the_ssh_key_is_read_without_the_credentials() {
        with_clean_env(|| {
            let (_dir, paths) = make_paths();
            let key_dir = tempdir().unwrap();
            let key_path = key_dir.path().join("file.pub");
            std::fs::write(&key_path, "ssh-ed25519 AAAAZmlsZQ== me@file\n").unwrap();
            save_target(
                &paths,
                &Target {
                    name: "default".into(),
                    config: TargetConfig {
                        provider: "hetzner-cloud".into(),
                        ssh_key_path: Some(key_path),
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
            std::fs::write(paths.target_credentials_file("default"), "{not yaml").unwrap();
            assert_eq!(
                resolve_hetzner_ssh_public_key(&paths, None)
                    .unwrap()
                    .as_deref(),
                Some("ssh-ed25519 AAAAZmlsZQ== me@file")
            );
        });
    }
}
