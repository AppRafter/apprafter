// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! 2.6d backup/restore: native export + restic-wrapped local-pull backup +
//! restore. CLI-orchestrated; cluster-scoped by default.

pub mod extract;
pub mod helper_pod;

// Pure modules now live in backup-core; re-export so existing import paths
// (cli_providers::backup::{restic, manifest, …}) keep resolving.
pub use backup_core::{images, manifest, reseal, restic, restore, sanitize};

// Shared types defined in backup-core; re-export here for callers that import
// them via cli_providers::backup::{DataKind, ResourceRef}.
pub use backup_core::{DataKind, ResourceRef};
