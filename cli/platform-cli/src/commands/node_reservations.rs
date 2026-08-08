// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter node reserve-headroom` — retrofit the 2.16d kubelet
//! node reservations + k3s OOM-protection onto a cluster that was
//! provisioned before they shipped at bootstrap.
//!
//! Bootstrap (Task 10, `cli-providers::hetzner_cloud::user_data`)
//! bakes `/etc/rancher/k3s/config.yaml` (system/kube-reserved +
//! eviction-hard) and the `k3s.service.d/oom.conf` drop-in into
//! cloud-init, so every freshly-provisioned node has them from the
//! first boot. Clusters created earlier don't — the scheduler
//! over-commits (k3s.service, ~1.5G in system.slice, is invisible
//! to per-pod requests) and `k3s.service` sits at the default
//! `OOMScoreAdjust=0`, as OOM-killable as any pod. This subcommand
//! closes that gap on a live node:
//!
//! 1. SSH the active target's node (reusing the exact identity +
//!    known-hosts posture `apprafter kubeconfig` uses — the shared
//!    [`SshCommandRunner`] and [`default_ssh_identity_path`]).
//! 2. Write the **same** config + drop-in the bootstrap path ships
//!    ([`k3s_reservation_config`] / [`K3S_OOM_DROPIN`] are the DRY
//!    single source of truth for both).
//! 3. `systemctl daemon-reload` + `systemctl restart k3s` so the
//!    kubelet re-reads config.yaml and the unit picks up the
//!    drop-in.
//! 4. Poll for the API to come back (restart takes it offline ~30s).
//!
//! Gated behind a confirmation prompt (the k3s restart is briefly
//! disruptive) unless `--yes`.

use std::time::{Duration, Instant};

use cli_core::{resolve_hetzner_token, CliError, Result};
use cli_providers::hetzner_cloud::user_data::{
    k3s_reservation_config, K3S_CONFIG_PATH, K3S_OOM_DROPIN, K3S_OOM_DROPIN_PATH,
};
use cli_providers::hetzner_cloud::{default_ssh_identity_path, SshCommandRunner};
use cli_providers::{node_public_ips, HetznerCloudClient};
use cli_state::State;
use tracing::info;

use crate::cli::NodeAction;
use crate::commands::hcloud::hcloud_base_url;
use crate::commands::state_paths::resolve_state_paths;

/// How long to wait for the k3s API to come back after the restart
/// before declaring the retrofit failed. A single-node k3s restart
/// is typically back in ~20-30s; 180s is generous headroom.
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(180);
/// Poll interval while waiting for the API to recover.
const RECOVERY_INTERVAL: Duration = Duration::from_secs(5);

pub fn run(action: NodeAction) -> Result<()> {
    match action {
        NodeAction::ReserveHeadroom { yes } => reserve_headroom(yes),
    }
}

/// Builds the remote shell script that lands both files atomically
/// and restarts k3s. Pure and side-effect-free — this is the unit
/// under test; `reserve_headroom` only wraps it in SSH + prompts +
/// recovery polling.
///
/// The two file bodies come verbatim from the shared bootstrap
/// builders, so the retrofit and the fresh-install path can never
/// drift. Each file is written via a quoted heredoc (`<<'EOF'`) so
/// the shell performs NO expansion on the body — the `<` in
/// `eviction-hard=memory.available<100Mi`, the `[Service]` header,
/// and every quote survive untouched. `mkdir -p` first because the
/// drop-in dir (`k3s.service.d`) may not exist on an old node.
pub fn remote_setup_script() -> String {
    let config_body = k3s_reservation_config();
    let oom_dir = std::path::Path::new(K3S_OOM_DROPIN_PATH)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("/etc/systemd/system/k3s.service.d");
    let config_dir = std::path::Path::new(K3S_CONFIG_PATH)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("/etc/rancher/k3s");

    format!(
        "set -e\n\
         mkdir -p {config_dir} {oom_dir}\n\
         cat > {config_path} <<'APPRAFTER_K3S_CFG_EOF'\n\
         {config_body}\
         APPRAFTER_K3S_CFG_EOF\n\
         cat > {oom_path} <<'APPRAFTER_K3S_OOM_EOF'\n\
         {oom_body}\
         APPRAFTER_K3S_OOM_EOF\n\
         systemctl daemon-reload\n\
         systemctl restart k3s\n",
        config_dir = config_dir,
        oom_dir = oom_dir,
        config_path = K3S_CONFIG_PATH,
        config_body = config_body,
        oom_path = K3S_OOM_DROPIN_PATH,
        oom_body = K3S_OOM_DROPIN,
    )
}

/// Remote command that succeeds once the k3s API is serving again.
/// `k3s kubectl` uses the node-local kubeconfig, so this needs no
/// external creds; a non-zero exit (API still starting) drives the
/// caller's retry loop.
fn recovery_probe_command() -> &'static str {
    "k3s kubectl get --raw='/readyz'"
}

fn reserve_headroom(yes: bool) -> Result<()> {
    info!(yes, "node reserve-headroom invoked");

    let resolved = resolve_state_paths(None)?;
    let paths = resolved.paths;
    let store = resolved.store;
    let state = State::load_or_default(&paths)?;

    let Some(server_id) = state.hetzner_cloud.as_ref().map(|h| h.server_id) else {
        return Err(CliError::Other(
            "no provisioned server for the active target — run `apprafter up` first".into(),
        ));
    };

    // Resolve the node's public IPv4 via the live Hetzner API (same
    // path `apprafter target ip` / `kubeconfig` use).
    let token = resolve_hetzner_token(None, &store, None)?;
    let client = HetznerCloudClient::new(hcloud_base_url(), token);
    let (v4, _v6) = node_public_ips(&client, server_id)?;
    let host = v4.ok_or_else(|| {
        CliError::Other(
            "the active target's node has no public IPv4 yet — wait for cloud-init".into(),
        )
    })?;

    if !yes {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` to skip the confirmation prompt".into(),
            ));
        }
        println!(
            "This restarts k3s on {host} (~30s, API briefly unavailable) to apply node \
             reservations (system-reserved=1500Mi, kube-reserved, eviction-hard) and the \
             OOMScoreAdjust=-999 drop-in."
        );
        let confirmed = inquire::Confirm::new("Continue?")
            .with_default(false)
            .prompt()
            .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
        if !confirmed {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let runner = SshCommandRunner::new(default_ssh_identity_path(), paths.known_hosts_file());

    println!("Applying node reservations on {host} over SSH…");
    runner.run(&host, &remote_setup_script())?;
    println!("Config + OOM drop-in written; k3s restarting. Waiting for the API to recover…");

    wait_for_recovery(&runner, &host)?;

    println!("✓ Node reservations applied and the k3s API is back.");
    println!("  system-reserved=memory=1500Mi (covers k3s.service), kube-reserved, eviction-hard.");
    println!("  k3s.service now runs at OOMScoreAdjust=-999.");
    Ok(())
}

/// Polls the node until `k3s kubectl get --raw=/readyz` succeeds or
/// the timeout elapses.
fn wait_for_recovery(runner: &SshCommandRunner, host: &str) -> Result<()> {
    let deadline = Instant::now() + RECOVERY_TIMEOUT;
    let mut last_err;
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match runner.run(host, recovery_probe_command()) {
            Ok(_) => return Ok(()),
            Err(e) => last_err = e.to_string(),
        }
        if Instant::now() >= deadline {
            return Err(CliError::Other(format!(
                "k3s API did not recover within {}s after restart ({attempt} attempts). \
                 The node reservations were written; SSH in and check `systemctl status k3s`. \
                 Last probe error: {last_err}",
                RECOVERY_TIMEOUT.as_secs()
            )));
        }
        std::thread::sleep(RECOVERY_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_setup_script_embeds_shared_reservation_config() {
        let script = remote_setup_script();
        // DRY: the exact bytes of the shared bootstrap builder must
        // appear in the retrofit script (single source of truth).
        assert!(
            script.contains(&k3s_reservation_config()),
            "retrofit script must embed the shared k3s_reservation_config() verbatim:\n{script}"
        );
        assert!(
            script.contains(K3S_OOM_DROPIN),
            "retrofit script must embed the shared K3S_OOM_DROPIN verbatim:\n{script}"
        );
    }

    #[test]
    fn remote_setup_script_writes_both_files_to_canonical_paths() {
        let script = remote_setup_script();
        assert!(script.contains(K3S_CONFIG_PATH), "{script}");
        assert!(script.contains(K3S_OOM_DROPIN_PATH), "{script}");
    }

    #[test]
    fn remote_setup_script_preserves_eviction_less_than() {
        // The `<` must survive into the remote file — a quoted
        // heredoc performs no shell expansion, so it stays literal.
        let script = remote_setup_script();
        assert!(
            script.contains("eviction-hard=memory.available<100Mi"),
            "the eviction-hard `<` must be present literally (quoted heredoc, no expansion):\n{script}"
        );
    }

    #[test]
    fn remote_setup_script_uses_quoted_heredocs_and_restarts_k3s() {
        let script = remote_setup_script();
        // Quoted heredoc delimiter → no expansion of the body.
        assert!(script.contains("<<'APPRAFTER_K3S_CFG_EOF'"), "{script}");
        assert!(script.contains("<<'APPRAFTER_K3S_OOM_EOF'"), "{script}");
        assert!(script.contains("systemctl daemon-reload"), "{script}");
        assert!(script.contains("systemctl restart k3s"), "{script}");
        // mkdir -p so the drop-in dir exists on an old node.
        assert!(script.contains("mkdir -p"), "{script}");
        // set -e so a failed write aborts before the restart.
        assert!(script.starts_with("set -e\n"), "{script}");
    }

    #[test]
    fn recovery_probe_uses_node_local_k3s_kubectl() {
        assert_eq!(recovery_probe_command(), "k3s kubectl get --raw='/readyz'");
    }
}
