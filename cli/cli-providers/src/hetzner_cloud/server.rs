// SPDX-License-Identifier: FSL-1.1-MIT
//! Higher-level server helpers: ServerSpec carries the desired
//! shape of a server in a way the provider can diff against the
//! API's `Server` view.

use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct ServerSpec {
    pub name: String,
    pub server_type: String,
    pub image: String,
    pub location: String,
    pub labels: BTreeMap<String, String>,
}

/// Tag every AppRafter-managed Hetzner resource with this label so
/// `import` / `destroy` can find them later.
pub const APPRAFTER_LABEL: &str = "apprafter";
pub const APPRAFTER_LABEL_VALUE: &str = "true";
