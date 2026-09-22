// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure env-config parser for the in-cluster backup runner.
//!
//! [`RunnerConfig::from_env_map`] is a pure function that takes a
//! `BTreeMap<String, String>` (testable without touching the real process env).
//! [`RunnerConfig::from_env`] is the thin impure wrapper used by `main`.

use std::collections::BTreeMap;
use std::time::Duration;

use cli_core::{CliError, Result};

/// The restic `--host` a run uses when the cluster carries no
/// `spec.backup.clusterName`. Fixed rather than the pod name, which is
/// ephemeral (spec §Retention M-r3-1a), and unchanged from what every
/// pre-`clusterName` cluster has been writing.
pub const DEFAULT_BACKUP_HOST: &str = "apprafter-backup";

/// Full resolved configuration for one backup runner invocation.
pub struct RunnerConfig {
    /// Restic repository URL, e.g. `s3:https://endpoint/bucket`.
    pub repo: String,
    /// HUMAN cluster label, stamped into the backup manifest's `clusterId` and
    /// the failure-webhook payload. `spec.backup.clusterName` when the operator
    /// set one, else the Helm release name. NOT the snapshot's identity — that
    /// is the `kube-system` UID the runner reads at run time (E1).
    pub cluster_id: String,
    /// restic `--host` for every snapshot of the run: the human cluster name,
    /// so a listing is legible and groupable. Defaults to the fixed
    /// `apprafter-backup` on a cluster that has not been named.
    pub backup_host: String,
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
    /// The run's deadline: the Job's `activeDeadlineSeconds`, which the chart
    /// renders into `APPRAFTER_BACKUP_DEADLINE_SECONDS` from the same value.
    /// Kubernetes stops the run there with SIGTERM; the runner uses it to say
    /// so, and keeps its helper pods alive for exactly this long. `None` when
    /// the variable is absent — a Job template older than it.
    pub deadline: Option<Duration>,
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

        // Optional and defaulted rather than required: a cluster whose chart
        // predates `clusterName` keeps the fixed host it has always used, so
        // upgrading the runner never silently re-groups an existing repository.
        let backup_host = e
            .get("APPRAFTER_BACKUP_HOST")
            .filter(|s| !s.is_empty())
            .cloned()
            .unwrap_or_else(|| DEFAULT_BACKUP_HOST.to_string());

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

        let deadline = match e.get("APPRAFTER_BACKUP_DEADLINE_SECONDS") {
            None => None,
            Some(v) => Some(parse_deadline(v)?),
        };

        Ok(RunnerConfig {
            repo,
            cluster_id,
            backup_host,
            passphrase,
            staging_mode,
            enforce_in_cluster,
            retention,
            failure_webhook,
            deadline,
        })
    }

    /// Build a [`RunnerConfig`] from the real process environment.
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

/// `APPRAFTER_BACKUP_DEADLINE_SECONDS`: whole seconds, above zero. The chart
/// renders the Job's `activeDeadlineSeconds` here, which its schema already
/// holds to ten minutes or more; anything unreadable is a broken render, and a
/// precondition error says so rather than running with no deadline at all.
fn parse_deadline(value: &str) -> Result<Duration> {
    match value.parse::<u64>() {
        Ok(secs) if secs > 0 => Ok(Duration::from_secs(secs)),
        _ => Err(CliError::Other(format!(
            "env APPRAFTER_BACKUP_DEADLINE_SECONDS={value:?} is not a whole number of seconds \
             above zero"
        ))),
    }
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
            ("APPRAFTER_BACKUP_HOST", "prod"),
            ("APPRAFTER_BACKUP_FAILURE_WEBHOOK", "https://hook"),
            ("APPRAFTER_BACKUP_DEADLINE_SECONDS", "21600"),
        ]);
        let c = RunnerConfig::from_env_map(&e).unwrap();
        assert_eq!(c.repo, "s3:https://ep/b");
        assert_eq!(c.cluster_id, "c1");
        assert_eq!(c.backup_host, "prod");
        assert_eq!(c.passphrase, "p");
        assert_eq!(c.staging_mode, backup_core::StagingMode::Sequential);
        assert!(c.enforce_in_cluster);
        assert_eq!(c.retention.keep_daily, 5);
        assert_eq!(c.failure_webhook.as_deref(), Some("https://hook"));
        assert_eq!(c.deadline, Some(Duration::from_secs(21600)));
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
        // An unnamed cluster keeps the host it has always written under —
        // upgrading the runner must not re-group an existing repository.
        assert_eq!(c.backup_host, DEFAULT_BACKUP_HOST);
        assert!(!c.enforce_in_cluster);
        assert_eq!(c.retention.keep_daily, 7); // RetentionPolicy::default
        assert_eq!(c.retention.keep_weekly, 4);
        assert_eq!(c.retention.keep_monthly, 6);
        assert!(c.failure_webhook.is_none());
        assert!(c.deadline.is_none());
    }

    #[test]
    fn an_unreadable_deadline_is_a_precondition_error_not_no_deadline() {
        for bad in ["", "0", "-5", "6h", "21600.5", "abc"] {
            let e = map(&[
                ("APPRAFTER_BACKUP_REPO", "s3:x"),
                ("APPRAFTER_CLUSTER_ID", "c"),
                ("RESTIC_PASSWORD", "p"),
                ("APPRAFTER_BACKUP_DEADLINE_SECONDS", bad),
            ]);
            let err = RunnerConfig::from_env_map(&e)
                .err()
                .unwrap_or_else(|| panic!("{bad:?} must be refused"));
            assert!(
                err.to_string()
                    .contains("APPRAFTER_BACKUP_DEADLINE_SECONDS"),
                "{err}"
            );
        }
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
    fn an_empty_backup_host_falls_back_to_the_fixed_default() {
        // The chart renders the env unconditionally, so an unnamed cluster
        // sends an EMPTY string rather than omitting the variable.
        let e = map(&[
            ("APPRAFTER_BACKUP_REPO", "s3:x"),
            ("APPRAFTER_CLUSTER_ID", "c"),
            ("RESTIC_PASSWORD", "p"),
            ("APPRAFTER_BACKUP_HOST", ""),
        ]);
        let c = RunnerConfig::from_env_map(&e).unwrap();
        assert_eq!(c.backup_host, DEFAULT_BACKUP_HOST);
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
