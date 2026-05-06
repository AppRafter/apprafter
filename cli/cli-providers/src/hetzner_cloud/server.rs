// SPDX-License-Identifier: FSL-1.1-MIT
//! Higher-level resource specs: ServerSpec carries the desired
//! server shape; SshKeySpec carries the desired Hetzner SSH-key.
//! The provider diffs these specs against the API view.

use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct ServerSpec {
    pub name: String,
    pub server_type: String,
    pub image: String,
    pub location: String,
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct SshKeySpec {
    pub name: String,
    pub public_key: String,
}

/// Tag every AppRafter-managed Hetzner resource with this label so
/// `import` / `destroy` can find them later.
pub const APPRAFTER_LABEL: &str = "apprafter";
pub const APPRAFTER_LABEL_VALUE: &str = "true";
