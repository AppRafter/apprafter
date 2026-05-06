// SPDX-License-Identifier: FSL-1.1-MIT
//! Wire types for the Hetzner Cloud REST API.
//!
//! Only the subset needed by the CX22 server-CRUD flow is
//! covered. Network/firewall/SSH/floating-IP types arrive in the
//! follow-up cycles for plan.md phase 1.2.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerStatus {
    Running,
    Initializing,
    Starting,
    Stopping,
    Off,
    Deleting,
    Migrating,
    Rebuilding,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Server {
    pub id: u64,
    pub name: String,
    pub status: ServerStatus,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerListResponse {
    pub servers: Vec<Server>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerCreateRequest {
    pub name: String,
    pub server_type: String,
    pub image: String,
    pub location: String,
    pub labels: BTreeMap<String, String>,
    pub start_after_create: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerCreateResponse {
    pub server: Server,
    /// Hetzner returns a one-time root password unless SSH keys
    /// are passed at creation time (we don't yet — that's the next
    /// 1.2.x cycle).
    #[serde(default)]
    pub root_password: Option<String>,
}

/// Body of an error response from the Hetzner API.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiErrorEnvelope {
    pub error: ApiErrorDetails,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiErrorDetails {
    pub code: String,
    pub message: String,
}
