// SPDX-License-Identifier: FSL-1.1-MIT
//! Local state file for `platform-cli`.
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
