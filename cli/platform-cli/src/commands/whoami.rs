// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter whoami` — one-line summary of the operator's
//! current shell context: identity, active target, verified
//! status, and the config fields most operational commands care
//! about.
//!
//! Per `cli-dx-task.md` §5.8 the layout is human-scannable rather
//! than machine-parseable; structured output (JSON, etc.) lands
//! in a later iteration when Track A.11 picks up the
//! `--output=<fmt>` flag pass.
//!
//! Deliberately small surface — no flags beyond `--no-ping` —
//! so the command stays predictable in shell prompts and CI
//! status banners.

use std::path::Path;

use cli_core::target::{
    default_config_root, load_global_config, load_target, Target, TargetStorePaths,
};
use cli_core::{CliError, Result};
use cli_providers::{HetznerCloudValidator, ProviderValidator};
use tracing::info;

use crate::commands::hcloud::hcloud_base_url;

pub fn run(no_ping: bool) -> Result<()> {
    info!(no_ping, "whoami invoked");

    let paths = TargetStorePaths::for_root(default_config_root()?);
    let active_name = load_global_config(&paths)?
        .map(|g| g.active_target)
        .filter(|s| !s.is_empty());

    // Identity is hardcoded to "anonymous (self-hosted)" until
    // Track A.10+ wires AppRafter Cloud auth; the line is kept
    // here so future work is purely additive (just swap the
    // string source).
    println!("Identity:     anonymous (self-hosted mode)");

    let Some(name) = active_name else {
        println!();
        println!(
            "No active target. Run `apprafter target add` to create one — `apprafter target list` to see what's configured."
        );
        return Ok(());
    };

    let target = load_target(&paths, &name)?;

    println!("Target:       {name} (active)");
    let verified = verified_status(&target, no_ping);
    println!("Provider:     {} ({verified})", target.config.provider);
    println!(
        "Region:       {}",
        target.config.region.as_deref().unwrap_or("not set")
    );
    println!(
        "Server type:  {}",
        target.config.server_type.as_deref().unwrap_or("not set")
    );
    println!(
        "Default tier: {}",
        target.config.default_tier.as_deref().unwrap_or("not set")
    );
    println!(
        "Cluster name: {}",
        target.config.cluster_name.as_deref().unwrap_or("not set")
    );
    println!(
        "SSH key:      {}",
        ssh_key_status(target.config.ssh_key_path.as_deref())
    );
    println!();
    println!(
        "Run `apprafter target show` for the full target config; `apprafter target list` for all configured targets."
    );
    Ok(())
}

/// Render the per-line verified status. Best-effort: a failing
/// ping does NOT fail the whoami command itself — operators
/// running `whoami` on a flaky network shouldn't get an exit-1
/// when the rest of the info is still useful. Failures show as
/// `verification failed ✗ — <hint>`.
fn verified_status(target: &Target, no_ping: bool) -> String {
    if no_ping {
        return "verification skipped — --no-ping".to_string();
    }
    let Some(token) = target.credentials.hetzner_token.as_deref() else {
        return "verification skipped — no token stored".to_string();
    };
    match ping_target(target, token) {
        Ok(()) => "verified ✓".to_string(),
        Err(CliError::Hetzner { status: 401, .. }) => {
            "verification failed ✗ — token rejected (HTTP 401). Run `apprafter target add <name> --renew` to rotate.".to_string()
        }
        Err(CliError::Hetzner { status, .. }) => {
            format!("verification failed ✗ — HTTP {status} from provider API")
        }
        Err(_) => "verification failed ✗ — provider unreachable (network?)".to_string(),
    }
}

fn ping_target(target: &Target, token: &str) -> Result<()> {
    match target.config.provider.as_str() {
        "hetzner-cloud" => {
            let v = HetznerCloudValidator::new(hcloud_base_url(), token);
            v.validate_credentials()
        }
        other => Err(CliError::Other(format!(
            "no validator wired for provider `{other}`"
        ))),
    }
}

/// Render `~/.ssh/...` instead of an absolute path when the key
/// lives under `$HOME`, plus a `(loaded)` / `(missing!)` marker
/// so the operator can spot a stale config (deleted key file
/// that's still referenced in `config.yaml`) at a glance. `(not
/// set)` for the no-key case.
fn ssh_key_status(path: Option<&Path>) -> String {
    let Some(p) = path else {
        return "not set".to_string();
    };
    let display = abbreviate_home(p);
    if p.exists() {
        format!("{display} (loaded)")
    } else {
        format!("{display} (missing!)")
    }
}

fn abbreviate_home(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cli_core::target::{TargetConfig, TargetCredentials};

    fn target_with_token(token: Option<&str>) -> Target {
        Target {
            name: "t".into(),
            config: TargetConfig {
                provider: "hetzner-cloud".into(),
                ..Default::default()
            },
            credentials: TargetCredentials {
                hetzner_token: token.map(String::from),
            },
        }
    }

    #[test]
    fn verified_status_honours_no_ping_flag_without_hitting_network() {
        let t = target_with_token(Some("aaaaaaaaaaaaaaaa"));
        let s = verified_status(&t, true);
        assert!(s.contains("skipped"), "{s}");
        assert!(s.contains("--no-ping"), "{s}");
    }

    #[test]
    fn verified_status_reports_no_token_path_when_credentials_empty() {
        let t = target_with_token(None);
        let s = verified_status(&t, false);
        assert!(s.contains("no token stored"), "{s}");
    }

    #[test]
    fn ssh_key_status_shows_loaded_for_existing_path() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("id_ed25519.pub");
        std::fs::write(&key, "ssh-ed25519 AAAA").unwrap();
        let s = ssh_key_status(Some(&key));
        assert!(s.contains("(loaded)"), "{s}");
    }

    #[test]
    fn ssh_key_status_flags_missing_path_loudly() {
        let p = std::path::Path::new("/does/not/exist/key.pub");
        let s = ssh_key_status(Some(p));
        assert!(s.contains("(missing!)"), "{s}");
    }

    #[test]
    fn ssh_key_status_returns_not_set_for_none() {
        assert_eq!(ssh_key_status(None), "not set");
    }
}
