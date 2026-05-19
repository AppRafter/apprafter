// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Local state file for `apprafter`.
//!
//! State lives in `<repo>/.apprafter/state.json` and is currently
//! a JSON document. Encoding will switch to CUE-encoded text once
//! the schema stabilises (tracked in plan.md phase 1.x).
//! Encryption via age/sops is layered on top later (phase 3.x).

use std::path::{Path, PathBuf};

use cli_core::{CliError, Result, Tier};
use serde::{Deserialize, Serialize};

const STATE_DIR: &str = ".apprafter";
const STATE_FILE: &str = "state.json";
const KNOWN_HOSTS_FILE: &str = "known_hosts";

#[derive(Debug, Clone)]
pub struct StatePaths {
    root: PathBuf,
}

impl StatePaths {
    pub fn for_root(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    pub fn state_dir(&self) -> PathBuf {
        self.root.join(STATE_DIR)
    }

    pub fn state_file(&self) -> PathBuf {
        self.state_dir().join(STATE_FILE)
    }

    /// Per-cluster SSH `known_hosts` file. Lives next to
    /// `state.json` so it is naturally scoped to one cluster: when
    /// `destroy --yes` clears state, this file is removed too, and
    /// the next `apply` against a fresh server (which Hetzner may
    /// happily place at a recently-recycled IP) starts with a clean
    /// slate. Avoids the "host key verification failed" annoyance
    /// from the user's `~/.ssh/known_hosts` that would otherwise
    /// require manual `ssh-keygen -R` after every destroy+apply.
    pub fn known_hosts_file(&self) -> PathBuf {
        self.state_dir().join(KNOWN_HOSTS_FILE)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub cluster_name: Option<String>,
    pub tier: Option<Tier>,
    pub provider: Option<String>,
    pub region: Option<String>,
    pub hetzner_cloud: Option<HetznerCloudState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HetznerCloudState {
    pub server_id: u64,
    pub server_name: String,
    #[serde(default)]
    pub ssh_key_ids: Vec<u64>,
    #[serde(default)]
    pub network_id: Option<u64>,
    #[serde(default)]
    pub firewall_id: Option<u64>,
    #[serde(default)]
    pub floating_ip_ids: Vec<u64>,
    /// Plain-text k3s kubeconfig — legacy, kept for one cycle so
    /// existing state.json files still print something useful via
    /// `apprafter kubeconfig`. New caches go to
    /// `kubeconfig_age` instead.
    #[serde(default)]
    pub kubeconfig_yaml: Option<String>,
    /// age-encrypted (ASCII-armored) k3s kubeconfig. Populated by
    /// `apprafter kubeconfig` on cold-fetch and re-fetch.
    #[serde(default)]
    pub kubeconfig_age: Option<String>,
    /// age-encrypted (ASCII-armored) Argo CD admin password.
    /// Populated by `apprafter argocd-password` on first call.
    #[serde(default)]
    pub argocd_admin_password_age: Option<String>,
}

impl State {
    pub fn load_or_default(paths: &StatePaths) -> Result<Self> {
        let path = paths.state_file();
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = std::fs::read(&path)?;
        serde_json::from_slice(&bytes).map_err(|err| CliError::InvalidState {
            path,
            message: err.to_string(),
        })
    }

    pub fn save(&self, paths: &StatePaths) -> Result<()> {
        std::fs::create_dir_all(paths.state_dir())?;
        let bytes = serde_json::to_vec_pretty(self)?;
        std::fs::write(paths.state_file(), bytes)?;
        Ok(())
    }
}
