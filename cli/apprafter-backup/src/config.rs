// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure env-config parser for the in-cluster backup runner.
//!
//! [`RunnerConfig::from_env_map`] is a pure function that takes a
//! `BTreeMap<String, String>` (testable without touching the real process env).
//! [`RunnerConfig::from_env`] is the thin impure wrapper used by `main`.

use std::collections::BTreeMap;

use cli_core::{CliError, Result};

/// Full resolved configuration for one backup runner invocation.
// Fields are read by main in Task 7 (runner loop); not yet wired.
#[allow(dead_code)]
pub struct RunnerConfig {
    /// Restic repository URL, e.g. `s3:https://endpoint/bucket`.
    pub repo: String,
    /// Logical cluster identifier stamped onto backup tags.
    pub cluster_id: String,
    /// Restic repository passphrase (from `RESTIC_PASSWORD`).
    pub passphrase: String,
    /// Whether to run claims sequentially or as a single monolithic snapshot.
    pub staging_mode: backup_core::StagingMode,
    /// When true, the runner verifies it is running inside the target cluster.
    pub enforce_in_cluster: bool,
    /// How many daily/weekly/monthly run representatives to retain.
    pub retention: backup_core::prune::RetentionPolicy,
    /// Optional URL to POST on backup failure.
    pub failure_webhook: Option<String>,
}

impl RunnerConfig {
    /// Build a [`RunnerConfig`] from an explicit env map (pure, testable).
    ///
    /// Required keys: `APPRAFTER_BACKUP_REPO`, `APPRAFTER_CLUSTER_ID`,
    /// `RESTIC_PASSWORD`.  All others are optional with documented defaults.
    pub fn from_env_map(e: &BTreeMap<String, String>) -> Result<Self> {
        let repo = require(e, "APPRAFTER_BACKUP_REPO")?;
        let cluster_id = require(e, "APPRAFTER_CLUSTER_ID")?;
        let passphrase = require(e, "RESTIC_PASSWORD")?;

        let staging_mode =
            if e.get("APPRAFTER_BACKUP_STAGING_MODE").map(|s| s.as_str()) == Some("sequential") {
                backup_core::StagingMode::Sequential
            } else {
                backup_core::StagingMode::Monolithic
            };

        let enforce_in_cluster =
            e.get("APPRAFTER_BACKUP_ENFORCE").map(|s| s.as_str()) == Some("cluster");

        let mut retention = backup_core::prune::RetentionPolicy::default();
        if let Some(v) = e.get("APPRAFTER_BACKUP_KEEP_DAILY") {
            retention.keep_daily = parse_u32(v, "APPRAFTER_BACKUP_KEEP_DAILY")?;
        }
        if let Some(v) = e.get("APPRAFTER_BACKUP_KEEP_WEEKLY") {
            retention.keep_weekly = parse_u32(v, "APPRAFTER_BACKUP_KEEP_WEEKLY")?;
        }
        if let Some(v) = e.get("APPRAFTER_BACKUP_KEEP_MONTHLY") {
            retention.keep_monthly = parse_u32(v, "APPRAFTER_BACKUP_KEEP_MONTHLY")?;
        }

        let failure_webhook = e
            .get("APPRAFTER_BACKUP_FAILURE_WEBHOOK")
            .filter(|s| !s.is_empty())
            .cloned();

        Ok(RunnerConfig {
            repo,
            cluster_id,
            passphrase,
            staging_mode,
            enforce_in_cluster,
            retention,
            failure_webhook,
        })
    }

    /// Build a [`RunnerConfig`] from the real process environment.
    // Not yet called by main; will be wired in Task 7 (runner loop).
    #[allow(dead_code)]
    pub fn from_env() -> Result<Self> {
        let map: BTreeMap<String, String> = std::env::vars().collect();
        Self::from_env_map(&map)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn require(e: &BTreeMap<String, String>, key: &str) -> Result<String> {
    e.get(key)
        .cloned()
        .ok_or_else(|| CliError::Other(format!("missing required env: {key}")))
}

fn parse_u32(value: &str, key: &str) -> Result<u32> {
    value
        .parse::<u32>()
        .map_err(|_| CliError::Other(format!("env {key}={value:?} is not a valid u32")))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parses_a_full_config() {
        let e = map(&[
            ("APPRAFTER_BACKUP_REPO", "s3:https://ep/b"),
            ("APPRAFTER_CLUSTER_ID", "c1"),
            ("APPRAFTER_BACKUP_STAGING_MODE", "sequential"),
            ("APPRAFTER_BACKUP_ENFORCE", "cluster"),
            ("APPRAFTER_BACKUP_KEEP_DAILY", "5"),
            ("RESTIC_PASSWORD", "p"),
            ("APPRAFTER_BACKUP_FAILURE_WEBHOOK", "https://hook"),
        ]);
        let c = RunnerConfig::from_env_map(&e).unwrap();
        assert_eq!(c.repo, "s3:https://ep/b");
        assert_eq!(c.cluster_id, "c1");
        assert_eq!(c.passphrase, "p");
        assert_eq!(c.staging_mode, backup_core::StagingMode::Sequential);
        assert!(c.enforce_in_cluster);
        assert_eq!(c.retention.keep_daily, 5);
        assert_eq!(c.failure_webhook.as_deref(), Some("https://hook"));
    }

    #[test]
    fn defaults_when_absent() {
        let e = map(&[
            ("APPRAFTER_BACKUP_REPO", "s3:x"),
            ("APPRAFTER_CLUSTER_ID", "c"),
            ("RESTIC_PASSWORD", "p"),
        ]);
        let c = RunnerConfig::from_env_map(&e).unwrap();
        assert_eq!(c.staging_mode, backup_core::StagingMode::Monolithic);
        assert!(!c.enforce_in_cluster);
        assert_eq!(c.retention.keep_daily, 7); // RetentionPolicy::default
        assert_eq!(c.retention.keep_weekly, 4);
        assert_eq!(c.retention.keep_monthly, 6);
        assert!(c.failure_webhook.is_none());
    }

    #[test]
    fn missing_required_fields_error() {
        // no repo
        assert!(RunnerConfig::from_env_map(&map(&[
            ("APPRAFTER_CLUSTER_ID", "c"),
            ("RESTIC_PASSWORD", "p")
        ]))
        .is_err());
        // no cluster_id
        assert!(RunnerConfig::from_env_map(&map(&[
            ("APPRAFTER_BACKUP_REPO", "s3:x"),
            ("RESTIC_PASSWORD", "p")
        ]))
        .is_err());
        // no passphrase
        assert!(RunnerConfig::from_env_map(&map(&[
            ("APPRAFTER_BACKUP_REPO", "s3:x"),
            ("APPRAFTER_CLUSTER_ID", "c")
        ]))
        .is_err());
    }

    #[test]
    fn non_numeric_retention_is_an_error() {
        let e = map(&[
            ("APPRAFTER_BACKUP_REPO", "s3:x"),
            ("APPRAFTER_CLUSTER_ID", "c"),
            ("RESTIC_PASSWORD", "p"),
            ("APPRAFTER_BACKUP_KEEP_DAILY", "not-a-number"),
        ]);
        assert!(RunnerConfig::from_env_map(&e).is_err());
    }

    #[test]
    fn empty_failure_webhook_is_none() {
        let e = map(&[
            ("APPRAFTER_BACKUP_REPO", "s3:x"),
            ("APPRAFTER_CLUSTER_ID", "c"),
            ("RESTIC_PASSWORD", "p"),
            ("APPRAFTER_BACKUP_FAILURE_WEBHOOK", ""),
        ]);
        let c = RunnerConfig::from_env_map(&e).unwrap();
        assert!(c.failure_webhook.is_none());
    }
}
